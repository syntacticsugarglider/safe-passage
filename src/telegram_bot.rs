use chrono::prelude::*;
use chrono_english::{parse_date_string, Dialect};
use ezviz::{camera_stream, EzvizApi};
use futures::{lock::Mutex, pin_mut, StreamExt};
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::env;
use std::io::Write;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use telegram_bot::*;

fn make_predicate(query: String) -> Box<dyn FnMut(DateTime<Utc>) -> bool + Send> {
    query
        .split(',')
        .map(|data| {
            let mut data = data.split(' ');
            let name = data.next()?;
            fn time_predicate<'a>(
                data: impl Iterator<Item = &'a str>,
                call: impl Fn(DateTime<Utc>, DateTime<Utc>) -> bool + Send + 'static,
            ) -> Option<Box<dyn Fn(DateTime<Utc>) -> bool + Send>> {
                let date: DateTime<Utc> = parse_date_string(
                    &data.collect::<Vec<_>>().join(" "),
                    Local::now(),
                    Dialect::Us,
                )
                .map_err(|e| {
                    eprintln!("{:?}", e);
                })
                .ok()?
                .into();
                Some(Box::new(move |sample: DateTime<Utc>| call(sample, date)))
            }
            match name {
                "after" | "since" => Some(Box::new(time_predicate(data, |c, arg| c > arg)?)
                    as Box<dyn FnMut(_) -> _ + Send>),
                "before" | "preceding" => Some(Box::new(time_predicate(data, |c, arg| c < arg)?)
                    as Box<dyn FnMut(_) -> _ + Send>),
                _ => None,
            }
        })
        .collect::<Option<Vec<_>>>()
        .map(|mut closures| {
            Box::new(move |item: DateTime<Utc>| {
                !closures.iter_mut().any(
                    |closure: &mut Box<dyn FnMut(DateTime<Utc>) -> bool + Send>| !closure(item),
                )
            }) as Box<dyn FnMut(_) -> _ + Send>
        })
        .unwrap_or(Box::new(|_| false) as Box<dyn FnMut(_) -> _ + Send>)
}

#[tokio::main]
async fn main() {
    let db = Arc::new(Mutex::new(sled::open("photo_ids").unwrap()));
    let frequency = std::env::var("CAPTURE_FREQUENCY")
        .expect("env var CAPTURE_FREQUENCY not set")
        .parse::<usize>()
        .unwrap();
    let photos = Arc::new(Mutex::new(
        db.lock()
            .await
            .iter()
            .filter_map(|data| {
                data.ok().map(|(time, photo)| {
                    (
                        DateTime::from_utc(
                            NaiveDateTime::from_timestamp(
                                i64::from_le_bytes(time.as_ref().try_into().unwrap()),
                                0,
                            ),
                            Utc,
                        ),
                        String::from_utf8(photo.as_ref().to_vec()).unwrap(),
                    )
                })
            })
            .collect::<BTreeMap<DateTime<Utc>, String>>(),
    ));
    let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
    let api = Arc::new(Mutex::new(telegram_bot::Api::new(token.clone())));
    let group = GroupId::new(
        env::var("TELEGRAM_GROUP_ID")
            .expect("TELEGRAM_GROUP_ID not set")
            .parse()
            .unwrap(),
    );
    let mut stream = api.lock().await.stream();
    tokio::spawn({
        let api = api.clone();
        let photos = photos.clone();
        async move {
            while let Some(Ok(update)) = stream.next().await {
                match update.kind {
                    UpdateKind::Message(message) => match &message.kind {
                        MessageKind::Text {
                            data: query,
                            entities: _,
                        } => {
                            let chat = message.chat;
                            if chat.to_chat_ref() != group.to_chat_ref() {
                                continue;
                            }
                            let chat = message.from;
                            let api = api.clone();
                            let mut predicate = make_predicate(query.into());
                            let photos = photos.clone();
                            let token = token.clone();
                            tokio::spawn(async move {
                                let em = api
                                    .lock()
                                    .await
                                    .send(SendMessage::new(chat.clone(), "Building zip: 0/0"))
                                    .await
                                    .unwrap();
                                let _ = api
                                    .lock()
                                    .await
                                    .send(SendChatAction::new(
                                        chat.clone(),
                                        ChatAction::UploadDocument,
                                    ))
                                    .await;
                                let stream = {
                                    photos
                                        .lock()
                                        .await
                                        .clone()
                                        .into_iter()
                                        .filter_map(|(timestamp, photo)| {
                                            if predicate(timestamp.clone()) {
                                                Some((photo, timestamp))
                                            } else {
                                                None
                                            }
                                        })
                                        .map({
                                            let api = api.clone();
                                            move |(photo, timestamp)| {
                                                let api = api.clone();
                                                let token = token.clone();
                                                async move {
                                                    let file = api
                                                        .lock()
                                                        .await
                                                        .send(GetFile::new(PhotoSize {
                                                            width: 1280,
                                                            file_id: photo.to_owned(),
                                                            height: 720,
                                                            file_size: None,
                                                        }))
                                                        .await
                                                        .unwrap();
                                                    let uri = format!(
                                                        "https://api.telegram.org/file/bot{}/{}",
                                                        token.clone(),
                                                        file.file_path.unwrap()
                                                    );
                                                    (
                                                        surf::get(uri)
                                                            .send()
                                                            .await
                                                            .unwrap()
                                                            .take_body()
                                                            .into_bytes()
                                                            .await
                                                            .unwrap(),
                                                        timestamp,
                                                    )
                                                }
                                            }
                                        })
                                }
                                .collect::<futures::stream::FuturesUnordered<_>>();
                                let total = stream.len();
                                let _ = api
                                    .lock()
                                    .await
                                    .send(EditMessageText::new(
                                        chat.clone(),
                                        em.clone(),
                                        format!("Building zip: 0/{}", total),
                                    ))
                                    .await;
                                pin_mut!(stream);
                                let mut buffer = uuid::Uuid::encode_buffer();
                                let file_name = format!(
                                    "{}.zip",
                                    uuid::Uuid::new_v4().to_simple().encode_lower(&mut buffer)
                                );
                                let file = std::fs::File::create(&file_name).unwrap();
                                let mut zip = zip::ZipWriter::new(file);
                                let options = zip::write::FileOptions::default()
                                    .compression_method(zip::CompressionMethod::DEFLATE);
                                let index = Arc::new(AtomicUsize::new(0));
                                while let Some((photo, timestamp)) = stream.next().await {
                                    zip.start_file(format!("{}.jpg", timestamp), options)
                                        .unwrap();
                                    zip.write_all(&photo).unwrap();
                                    tokio::spawn({
                                        let api = api.clone();
                                        let em = em.clone();
                                        let chat = chat.clone();
                                        let index = index.clone();
                                        async move {
                                            let _ = api
                                                .lock()
                                                .await
                                                .send(EditMessageText::new(
                                                    chat,
                                                    em,
                                                    format!(
                                                        "Building zip: {}/{}",
                                                        index.fetch_add(
                                                            1,
                                                            std::sync::atomic::Ordering::SeqCst
                                                        ),
                                                        total
                                                    ),
                                                ))
                                                .await;
                                        }
                                    });
                                }
                                zip.finish().unwrap();
                                let _ = api
                                    .lock()
                                    .await
                                    .send(EditMessageText::new(
                                        chat.clone(),
                                        em.clone(),
                                        format!("Uploading..."),
                                    ))
                                    .await;
                                api.lock()
                                    .await
                                    .send(SendDocument::new(
                                        chat.clone(),
                                        InputFileUpload::with_path(file_name.clone()),
                                    ))
                                    .await
                                    .unwrap();
                                let _ = api.lock().await.send(DeleteMessage::new(chat, em)).await;
                                std::fs::remove_file(file_name).unwrap();
                            });
                        }
                        _ => {}
                    },
                    UpdateKind::InlineQuery(query) => {
                        let api = api.clone();
                        let photos = photos.clone();
                        let mut predicate = make_predicate(query.query);
                        let id = query.id;
                        tokio::spawn(async move {
                            api.lock()
                                .await
                                .send(AnswerInlineQuery::new(
                                    id,
                                    photos
                                        .lock()
                                        .await
                                        .iter()
                                        .filter_map(|(timestamp, photo)| {
                                            if predicate(timestamp.clone()) {
                                                Some(photo)
                                            } else {
                                                None
                                            }
                                        })
                                        .enumerate()
                                        .map(|(idx, photo)| {
                                            InlineQueryResultCachedPhoto {
                                                id: format!("{}", idx),
                                                photo_file_id: photo.into(),
                                                title: None,
                                                description: None,
                                                caption: None,
                                                parse_mode: None,
                                                reply_markup: None,
                                                input_message_content: None,
                                            }
                                            .into()
                                        })
                                        .take(10)
                                        .collect(),
                                ))
                                .await
                                .unwrap_or_else(|e| eprintln!("{}", e));
                        });
                    }
                    _ => {}
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
        if index % frequency == 0 {
            Some(image)
        } else {
            None
        }
    });
    pin_mut!(images);
    while let Some(image) = images.next().await {
        let mut png_data = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut png_data, image::ImageOutputFormat::Png)
            .unwrap();
        let ul = InputFileUpload::with_data(png_data, "camera.png");
        tokio::spawn({
            let api = api.clone();
            let db = db.clone();
            let photos = photos.clone();
            async move {
                match api.lock().await.send(SendPhoto::new(&group, ul)).await {
                    Err(e) => eprintln!("{}", e),
                    Ok(message) => {
                        if let MessageKind::Photo {
                            mut data,
                            caption: _,
                            media_group_id: _,
                        } = message.kind
                        {
                            data.sort_by(|a, b| b.width.cmp(&a.width));
                            if let Some(photo) = data.into_iter().next() {
                                let time = Utc::now();
                                photos.lock().await.insert(time, photo.file_id.clone());
                                let _ = db.lock().await.insert(
                                    time.timestamp().to_le_bytes(),
                                    photo.file_id.as_str().as_bytes(),
                                );
                            }
                        }
                    }
                }
            }
        });
    }
}
