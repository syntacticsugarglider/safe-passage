use ezviz::{camera_stream, EzvizApi};
use futures::{lock::Mutex, pin_mut, StreamExt};
use std::collections::HashSet;
use std::{env, sync::Arc};
use telegram_bot::*;

#[tokio::main]
async fn main() {
    let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
    let api = telegram_bot::Api::new(token);
    let mut stream = api.stream();
    let chats = Arc::new(Mutex::new(HashSet::new()));

    tokio::spawn({
        let chats = chats.clone();
        async move {
            while let Some(update) = stream.next().await {
                if let UpdateKind::Message(message) = update.unwrap().kind {
                    chats.lock().await.insert(message.from);
                }
            }
        }
    });

    let addr = {
        let api = EzvizApi::connect(
            env::var("EZVIZ_ACCOUNT").expect("no EZVIZ_ACCOUNT env var specified"),
            env::var("EZVIZ_PASSWORD").expect("no EZVIZ_PASSWORD env var specified"),
        )
        .await
        .unwrap();
        api.devices().await.unwrap().first().unwrap().addr
    };
    let images = camera_stream(
        addr,
        env::var("EZVIZ_VERIFICATION_CODE").expect("no EZVIZ_VERIFICATION_CODE env var specified"),
    )
    .enumerate()
    .filter_map(|(index, image)| async move {
        if index % 5 == 0 {
            Some(image)
        } else {
            None
        }
    });
    pin_mut!(images);
    while let Some(image) = images.next().await {
        let mut png_data = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .resize(640, 320, image::imageops::FilterType::Nearest)
            .write_to(&mut png_data, image::ImageOutputFormat::Png)
            .unwrap();
        let ul = InputFileUpload::with_data(png_data, "camera.png");
        for user in &*chats.lock().await {
            let req = api.send(SendPhoto::new(user, &ul));
            tokio::spawn(req);
        }
    }
}
