use futures::channel::mpsc::unbounded;
use futures::Stream;
use gst::gst_element_error;
use gst::prelude::*;
use serde::{de, Deserialize, Serialize};
use std::{
    collections::HashMap, convert::TryFrom, convert::TryInto, fmt::Debug, iter, net::IpAddr,
};
use surf::{Body, Response};
use thiserror::Error;

fn yuv420p_to_rgb(buf: &[u8]) -> Vec<u8> {
    let w = 1280;
    let h = 720;
    let total = w * h;
    let total_quad = total + total / 4;
    (0..h)
        .map(|y| (0..w).map(move |x| (x, y)))
        .flatten()
        .map(|(x, y)| {
            let co_y = buf[y * w + x] as f32;
            let offset = (y / 2) * (w / 2) + (x / 2);
            let co_u = buf[offset + total] as f32;
            let co_v = buf[offset + total_quad] as f32;
            let b = 1.164 * (co_y - 16.) + 2.018 * (co_u - 128.);
            let g = 1.164 * (co_y - 16.) - 0.813 * (co_v - 128.) - 0.391 * (co_u - 128.);
            let r = 1.164 * (co_y - 16.) + 1.596 * (co_v - 128.);
            iter::once(r as u8)
                .chain(iter::once(g as u8))
                .chain(iter::once(b as u8))
        })
        .flatten()
        .collect()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("error performing http request: {0}")]
    Http(surf::Error),
    #[error("incorrect username and/or password")]
    InvalidCredentials,
    #[error("invalid API domain")]
    InvalidApiDomain,
    #[error("the server did not provide a session ID")]
    NoSessionId,
    #[error("the server did not provide an IP address for `{0}`")]
    NoIpForDevice(String),
}

impl From<surf::Error> for Error {
    fn from(error: surf::Error) -> Self {
        Error::Http(error)
    }
}

#[derive(Debug)]
struct EzvizFeatureCode;

impl Serialize for EzvizFeatureCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        "92c579faa0902cbfcfcc4fc004ef67e7".serialize(serializer)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginPayload {
    account: String,
    password: String,
    feature_code: EzvizFeatureCode,
}

#[derive(Debug)]
enum ResponseCode {
    RegionRedirect,
    Success,
}

impl<'de> Deserialize<'de> for ResponseCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
        D::Error: de::Error,
    {
        Ok(match i32::deserialize(deserializer)? {
            1100 => ResponseCode::RegionRedirect,
            200 => ResponseCode::Success,
            e => {
                return Err(<D::Error as de::Error>::custom(format!(
                    "unknown response code {}",
                    e
                )))
            }
        })
    }
}

#[derive(Debug, Deserialize)]
struct MetaResponse {
    code: ResponseCode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginAreaResponse {
    api_domain: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    meta: MetaResponse,
    login_area: LoginAreaResponse,
    #[serde(default)]
    login_session: Option<SessionResponse>,
}

#[derive(Debug)]
pub struct EzvizApi {
    session_id: String,
    login_payload: LoginPayload,
    api_domain: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    camera_name: String,
    device_serial: String,
}

#[derive(Debug, Hash, Eq, PartialEq, Deserialize)]
#[serde(transparent)]
struct CameraRef {
    device_serial: String,
}

impl<'a> From<&'a Camera> for CameraRef {
    fn from(cam: &'a Camera) -> Self {
        CameraRef {
            device_serial: cam.device_serial.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    local_ip: IpAddr,
    net_ip: IpAddr,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevicesResponse {
    camera_infos: Vec<Camera>,
    connection_infos: HashMap<CameraRef, Connection>,
}

#[derive(Debug)]
pub struct Device {
    pub name: String,
    pub addr: IpAddr,
}

impl TryFrom<DevicesResponse> for Vec<Device> {
    type Error = Error;

    fn try_from(value: DevicesResponse) -> Result<Self, Self::Error> {
        Ok(value
            .camera_infos
            .iter()
            .map(|item| {
                Ok(Device {
                    name: item.camera_name.clone(),
                    addr: value
                        .connection_infos
                        .get(&item.into())
                        .ok_or_else(|| Error::NoIpForDevice(item.camera_name.clone()))?
                        .local_ip,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?)
    }
}

#[derive(Serialize)]
struct PageQuery {
    filter: String,
}

impl EzvizApi {
    async fn login(payload: &LoginPayload, subdomain: &str) -> Result<Response, Error> {
        Ok(surf::post(format!(
            "https://{}.ezvizlife.com/v3/users/login",
            subdomain
        ))
        .body(Body::from_form(&payload)?)
        .header("clientType", "1")
        .header("customNo", "1000001")
        .send()
        .await?)
    }
    pub async fn connect<T: AsRef<str>, U: AsRef<str>>(
        account: T,
        password: U,
    ) -> Result<Self, Error> {
        let login_payload = LoginPayload {
            account: account.as_ref().to_owned(),
            password: format!("{:x}", md5::compute(password.as_ref())),
            feature_code: EzvizFeatureCode,
        };
        let mut api_domain = "apiieu".to_owned();
        let mut response = EzvizApi::login(&login_payload, &api_domain).await?;
        if response.status() == 400 {
            Err(Error::InvalidCredentials)?;
        }
        let mut response: LoginResponse = response.body_json().await?;
        if let ResponseCode::RegionRedirect = response.meta.code {
            api_domain = response
                .login_area
                .api_domain
                .split('.')
                .next()
                .ok_or(Error::InvalidApiDomain)?
                .to_owned();
            response = EzvizApi::login(&login_payload, &api_domain)
                .await?
                .body_json()
                .await?;
        }
        Ok(EzvizApi {
            session_id: response.login_session.ok_or(Error::NoSessionId)?.session_id,
            login_payload,
            api_domain,
        })
    }
    pub async fn devices(&self) -> Result<Vec<Device>, Error> {
        Ok(surf::get(format!(
            "https://{}.ezvizlife.com/v3/userdevices/v1/devices/pagelist",
            self.api_domain
        ))
        .header("sessionId", &self.session_id)
        .query(&PageQuery {
            filter: "CLOUD,TIME_PLAN,CONNECTION,SWITCH,STATUS,WIFI,STATUS_EXT,NODISTURB,P2P,TTS,KMS,HIDDNS".to_owned()
        })?
        .recv_json::<DevicesResponse>()
        .await?.try_into()?)
    }
}

pub fn camera_stream(
    addr: IpAddr,
    verification_code: String,
) -> impl Stream<Item = image::ImageBuffer<image::Rgb<u8>, Vec<u8>>> {
    let (sender, receiver) = unbounded();
    let verification_code = verification_code.to_owned();
    std::thread::spawn(move || {
        gst::init().unwrap();
        let uri = format!(
            "rtsp://admin:{}@{}:554/h264_stream",
            verification_code, addr
        );
        let pipeline = gst::Pipeline::new(None);
        let src = gst::ElementFactory::make("rtspsrc", Some("source")).unwrap();

        src.set_property("location", &uri).unwrap();
        src.set_property("latency", &100u32).unwrap();

        let rtp_extract = gst::ElementFactory::make("rtph264depay", None).unwrap();
        let video_decode = gst::ElementFactory::make("avdec_h264", None).unwrap();
        let video_rate = gst::ElementFactory::make("videorate", None).unwrap();
        let video_convert = gst::ElementFactory::make("videoconvert", None).unwrap();

        video_rate.set_property("max-rate", &1).unwrap();
        video_rate.set_property("drop-only", &true).unwrap();

        let sink = gst::ElementFactory::make("appsink", None).unwrap();

        pipeline
            .add_many(&[
                &src,
                &rtp_extract,
                &video_decode,
                &video_rate,
                &video_convert,
                &sink,
            ])
            .unwrap();
        rtp_extract.link(&video_decode).unwrap();
        video_decode.link(&video_rate).unwrap();
        video_rate.link(&video_convert).unwrap();
        video_convert.link(&sink).unwrap();

        let appsink = sink
            .dynamic_cast::<gst_app::AppSink>()
            .expect("Sink element is expected to be an appsink!");
        appsink.set_caps(Some(&gst::Caps::new_simple("video/x-raw", &[])));

        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.get_buffer().ok_or_else(|| {
                        gst_element_error!(
                            appsink,
                            gst::ResourceError::Failed,
                            ("Failed to get buffer from appsink")
                        );

                        gst::FlowError::Error
                    })?;
                    let map = buffer.map_readable().map_err(|_| {
                        gst_element_error!(
                            appsink,
                            gst::ResourceError::Failed,
                            ("Failed to map buffer readable")
                        );

                        gst::FlowError::Error
                    })?;
                    sender
                        .unbounded_send(
                            image::ImageBuffer::<image::Rgb<u8>, Vec<u8>>::from_raw(
                                1280,
                                720,
                                yuv420p_to_rgb(map.as_slice()),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        src.connect_pad_added(move |_, src_pad| {
            let sink_pad = rtp_extract.get_static_pad("sink").unwrap();
            if !sink_pad.is_linked() {
                src_pad.link(&sink_pad).unwrap();
            }
        });

        pipeline.set_state(gst::State::Playing).unwrap();
        let bus = pipeline
            .get_bus()
            .expect("Pipeline without bus. Shouldn't happen!");
        bus.iter_timed(gst::CLOCK_TIME_NONE).for_each(drop);
        pipeline
            .set_state(gst::State::Null)
            .expect("Unable to set the pipeline to the `Null` state");
    });

    receiver
}
