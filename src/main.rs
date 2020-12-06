use ezviz::EzvizApi;
use gst::prelude::*;
use smol::block_on;
use std::env;

fn main() {
    block_on(async {
        let api = EzvizApi::connect(
            env::var("EZVIZ_ACCOUNT").expect("no EZVIZ_ACCOUNT env var specified"),
            env::var("EZVIZ_PASSWORD").expect("no EZVIZ_PASSWORD env var specified"),
        )
        .await
        .unwrap();
        let addr = api.devices().await.unwrap().first().unwrap().addr;
        gst::init().unwrap();
        let uri = format!(
            "rtsp://admin:{}@{}:554/h264_stream",
            env::var("EZVIZ_VERIFICATION_CODE")
                .expect("no EZVIZ_VERIFICATION_CODE env var specified"),
            addr
        );
        let playbin = gst::ElementFactory::make("playbin", None).unwrap();
        playbin.set_property("uri", &uri).unwrap();
        let bus = playbin.get_bus().unwrap();
        playbin.set_state(gst::State::Playing).unwrap();
        bus.iter_timed(gst::CLOCK_TIME_NONE).for_each(drop);
        playbin
            .set_state(gst::State::Null)
            .expect("Unable to set the pipeline to the `Null` state");
    })
}
