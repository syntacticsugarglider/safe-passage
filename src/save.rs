use ezviz::{camera_stream, EzvizApi};
use futures::StreamExt;
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
        let mut images = camera_stream(
            addr,
            env::var("EZVIZ_VERIFICATION_CODE")
                .expect("no EZVIZ_VERIFICATION_CODE env var specified"),
        );
        while let Some(image) = images.next().await {
            image.save("test.png").unwrap();
        }
    });
}
