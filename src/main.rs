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
        let pipeline = gst::Pipeline::new(None);
        let src = gst::ElementFactory::make("rtspsrc", Some("source")).unwrap();

        src.set_property("location", &uri).unwrap();
        src.set_property("latency", &100u32).unwrap();

        let rtp_extract = gst::ElementFactory::make("rtph264depay", None).unwrap();
        let video_decode = gst::ElementFactory::make("avdec_h264", None).unwrap();
        let sink = gst::ElementFactory::make("autovideosink", None).unwrap();
        pipeline
            .add_many(&[&src, &rtp_extract, &video_decode, &sink])
            .unwrap();
        rtp_extract.link(&video_decode).unwrap();
        video_decode.link(&sink).unwrap();

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
    })
}
