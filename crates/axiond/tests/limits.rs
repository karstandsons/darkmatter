//! What one person (or, for connections, anyone) can make the server hold:
//! uploads, pictures, emoji, websockets, key packages and connections, each
//! against a real axiond.

use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use futures_util::{SinkExt, StreamExt};
use mm_api::Client;
use reqwest::multipart::{Form, Part};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    app: App,
    dir: tempfile::TempDir,
}

async fn server_with(change: impl FnOnce(&mut Config)) -> Server {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config { data_dir: dir.path().into(), ..Config::default() };
    change(&mut config);
    let app = App::open(config).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, app, dir }
}

async fn login(s: &Server, name: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

async fn town_square(c: &Client) -> String {
    let team = c.my_teams().await.unwrap()[0].id.clone();
    c.my_channels(&team).await.unwrap().into_iter().find(|ch| ch.name == "town-square").unwrap().id
}

/// Posts a multipart form; the status and the error message, if any.
async fn post_form(s: &Server, c: &Client, path: &str, form: Form) -> (u16, String) {
    let resp = reqwest::Client::new().post(format!("{}/api/v4/{path}", s.url)).bearer_auth(c.token().unwrap()).multipart(form).send().await.unwrap();
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    (status, body["message"].as_str().unwrap_or_default().to_string())
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(w, h).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A PNG that says it's `w` × `h` and has no pixels: enough to read its
/// size, not to draw it.
fn png_header(w: u32, h: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for b in bytes {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        let body = [kind, data].concat();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    chunk(b"IHDR", &[w.to_be_bytes().as_slice(), &h.to_be_bytes(), &[8, 2, 0, 0, 0]].concat());
    // An empty zlib stream.
    chunk(b"IDAT", &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    chunk(b"IEND", &[]);
    out
}

fn stored_files(s: &Server) -> usize {
    fn count(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).map(|d| d.flatten().map(|e| if e.path().is_dir() { count(&e.path()) } else { 1 }).sum()).unwrap_or(0)
    }
    count(&s.dir.path().join("files"))
}

#[tokio::test]
async fn text_parts_of_an_upload_are_capped() {
    let s = server_with(|_| {}).await;
    let alice = login(&s, "alice").await;
    let town = town_square(&alice).await;
    let file = || Part::bytes(b"hello".to_vec()).file_name("a.txt");

    // A channel id or client id is short; one of a megabyte is refused
    // before it's read into memory.
    let long = Form::new().text("channel_id", "x".repeat(1024 * 1024)).part("files", file());
    assert_eq!(post_form(&s, &alice, "files", long).await.0, 400);
    let form = Form::new().text("channel_id", town.clone()).text("client_ids", "y".repeat(1024 * 1024)).part("files", file());
    assert_eq!(post_form(&s, &alice, "files", form).await.0, 400);
    let mut many = Form::new().text("channel_id", town.clone());
    for i in 0..11 {
        many = many.text("client_ids", format!("c{i}"));
    }
    assert_eq!(post_form(&s, &alice, "files", many.part("files", file())).await.0, 400);
    let ok = Form::new().text("channel_id", town.clone()).text("client_ids", "c1").part("files", file());
    assert_eq!(post_form(&s, &alice, "files", ok).await.0, 201);

    // The same for an emoji's name.
    let emoji = Form::new().part("image", Part::bytes(png(8, 8)).file_name("e.png")).text("emoji", format!(r#"{{"name":"{}"}}"#, "a".repeat(100_000)));
    assert_eq!(post_form(&s, &alice, "emoji", emoji).await.0, 400);
}

#[tokio::test]
async fn uploads_stop_at_the_quota_and_the_hourly_rate() {
    let s = server_with(|c| {
        c.upload_quota_mb = 1;
        c.uploads_per_hour = 4;
    })
    .await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let town = town_square(&alice).await;

    // 600 KB fits in 1 MB; another 600 KB doesn't, and isn't kept.
    alice.upload_file(&town, "a.bin", vec![1; 600 * 1024]).await.unwrap();
    let before = stored_files(&s);
    let err = alice.upload_file(&town, "b.bin", vec![1; 600 * 1024]).await.unwrap_err();
    assert!(format!("{err}").contains("limit of 1 MB"), "{err}");
    assert_eq!(stored_files(&s), before, "the refused upload was removed");
    // Someone else's quota is their own.
    bob.upload_file(&town, "b.bin", vec![1; 600 * 1024]).await.unwrap();

    // Four uploads an hour: two more small ones, then no more.
    alice.upload_file(&town, "c.txt", b"c".to_vec()).await.unwrap();
    alice.upload_file(&town, "d.txt", b"d".to_vec()).await.unwrap();
    let err = alice.upload_file(&town, "e.txt", b"e".to_vec()).await.unwrap_err();
    assert!(format!("{err:?}").contains("429"), "{err:?}");
}

#[tokio::test]
async fn pictures_with_too_many_pixels_are_not_decoded() {
    let s = server_with(|_| {}).await;
    let alice = login(&s, "alice").await;
    let alice_id = alice.me().await.unwrap().id;
    let town = town_square(&alice).await;

    // 7000 × 6000 is 42 megapixels: refused as a profile picture without
    // being decoded (the file has no pixels at all), and kept as a plain
    // file without previews.
    let bomb = png_header(7000, 6000);
    let form = Form::new().part("image", Part::bytes(bomb.clone()).file_name("me.png"));
    let (status, message) = post_form(&s, &alice, &format!("users/{alice_id}/image"), form).await;
    assert_eq!((status, message.contains("megapixels")), (400, true), "{message}");
    let up = alice.upload_file(&town, "bomb.png", bomb).await.unwrap();
    assert!(!up.file_infos[0].has_preview_image);

    // A real picture under the limit still gets its previews, and a picture
    // up to 10 MB (more than the usual 2 MB body) can be a profile picture.
    let up = alice.upload_file(&town, "ok.png", png(300, 200)).await.unwrap();
    assert!(up.file_infos[0].has_preview_image);
    let noise: Vec<u8> = (0..1500u32 * 1500 * 3).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let mut big = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_raw(1500, 1500, noise).unwrap().write_to(&mut big, image::ImageFormat::Png).unwrap();
    let big = big.into_inner();
    assert!(big.len() > 3 * 1024 * 1024, "{}", big.len());
    let form = Form::new().part("image", Part::bytes(big).file_name("me.png"));
    assert_eq!(post_form(&s, &alice, &format!("users/{alice_id}/image"), form).await.0, 200);
}

fn emoji(name: &str) -> Form {
    Form::new().part("image", Part::bytes(png(8, 8)).file_name("e.png")).text("emoji", format!(r#"{{"name":"{name}"}}"#))
}

#[tokio::test]
async fn emoji_can_be_for_admins_only() {
    let s = server_with(|c| c.emoji_admins_only = true).await;
    let (alice, bob) = (login(&s, "alice").await, login(&s, "bob").await);
    let (status, message) = post_form(&s, &bob, "emoji", emoji("party")).await;
    assert_eq!((status, message.as_str()), (403, "Only admins add custom emoji on this server."));
    assert_eq!(post_form(&s, &alice, "emoji", emoji("party")).await.0, 201);
}

#[tokio::test]
async fn emoji_are_limited_per_person() {
    let s = server_with(|c| c.emoji_per_account = 2).await;
    let (alice, bob) = (login(&s, "alice").await, login(&s, "bob").await);
    assert_eq!(post_form(&s, &bob, "emoji", emoji("one")).await.0, 201);
    assert_eq!(post_form(&s, &bob, "emoji", emoji("two")).await.0, 201);
    let (status, message) = post_form(&s, &bob, "emoji", emoji("three")).await;
    assert_eq!(status, 400, "{message}");
    // Deleting one makes room; admins aren't limited.
    let one = bob.custom_emoji(0, 200).await.unwrap().into_iter().find(|e| e.name == "one").unwrap().id;
    let del = reqwest::Client::new().delete(format!("{}/api/v4/emoji/{one}", s.url)).bearer_auth(bob.token().unwrap()).send().await.unwrap();
    assert_eq!(del.status(), 200);
    assert_eq!(post_form(&s, &bob, "emoji", emoji("three")).await.0, 201);
    for name in ["a", "b", "c"] {
        assert_eq!(post_form(&s, &alice, "emoji", emoji(name)).await.0, 201);
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn socket(s: &Server, c: &Client) -> Socket {
    let mut req = format!("{}/api/v4/websocket", s.url.replace("http://", "ws://")).into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {}", c.token().unwrap()).parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// Reads until the socket closes; false if it's still open after `wait`.
async fn closes(ws: &mut Socket, wait: Duration) -> bool {
    tokio::time::timeout(wait, async {
        while let Some(Ok(m)) = ws.next().await {
            if matches!(m, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn websockets_are_limited_in_number_size_and_rate() {
    let s = server_with(|_| {}).await;
    let bob = login(&s, "bob").await;
    let town = town_square(&bob).await;

    // Ten sockets at once: an eleventh closes the oldest.
    let mut sockets = Vec::new();
    for _ in 0..10 {
        sockets.push(socket(&s, &bob).await);
    }
    let _eleventh = socket(&s, &bob).await;
    assert!(closes(&mut sockets[0], Duration::from_secs(5)).await, "the oldest closed");
    assert!(!closes(&mut sockets[1], Duration::from_millis(300)).await, "the others stay");
    sockets.clear();

    // A message bigger than 64 KiB closes the socket.
    let mut ws = socket(&s, &bob).await;
    ws.send(Message::text("x".repeat(100 * 1024))).await.unwrap();
    assert!(closes(&mut ws, Duration::from_secs(5)).await);

    // Typing as fast as it can: the first 30 are acted on, the rest fail
    // until it slows down.
    let mut ws = socket(&s, &bob).await;
    for seq in 1..=60 {
        let typing = serde_json::json!({ "action": "user_typing", "seq": seq, "data": { "channel_id": town } });
        ws.send(Message::text(typing.to_string())).await.unwrap();
    }
    let (mut ok, mut failed) = (0, 0);
    while ok + failed < 60 {
        let Message::Text(text) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap() else { continue };
        let reply: serde_json::Value = serde_json::from_str(&text).unwrap();
        match reply["status"].as_str() {
            Some("OK") => ok += 1,
            Some("FAIL") => failed += 1,
            _ => {}
        }
    }
    assert!((30..=32).contains(&ok), "{ok} acted on");
    assert!(failed >= 28, "{failed} refused");
}

#[tokio::test]
async fn connections_are_capped_per_address() {
    let s = server_with(|c| c.max_connections_per_ip = 4).await;
    let addr = s.url.trim_start_matches("http://").to_string();
    let ping = || async {
        let mut c = tokio::net::TcpStream::connect(&addr).await.unwrap();
        c.write_all(b"GET /api/v4/system/ping HTTP/1.0\r\n\r\n").await.ok()?;
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut reply)).await.ok()?.ok()?;
        Some(String::from_utf8_lossy(&reply).into_owned())
    };
    assert!(ping().await.unwrap().starts_with("HTTP/1.0 200"));
    let mut idle = Vec::new();
    for _ in 0..4 {
        idle.push(tokio::net::TcpStream::connect(&addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ping().await.is_none_or(|r| r.is_empty()), "a fifth is refused");
    idle.pop();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ping().await.unwrap().starts_with("HTTP/1.0 200"), "room again");
}

#[tokio::test]
async fn slow_connections_are_dropped() {
    let s = server_with(|_| {}).await;
    let addr = s.url.trim_start_matches("http://").to_string();
    // Saying nothing, and sending headers that never end.
    let silent = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let mut slow = tokio::net::TcpStream::connect(&addr).await.unwrap();
    slow.write_all(b"GET /api/v4/system/ping HTTP/1.1\r\nHost: x\r\n").await.unwrap();
    let start = Instant::now();
    let dropped = |mut c: tokio::net::TcpStream| async move {
        let mut buf = [0u8; 256];
        loop {
            match tokio::time::timeout(Duration::from_secs(40), c.read(&mut buf)).await {
                Ok(Ok(0) | Err(_)) => return true,
                Ok(Ok(_)) => continue,
                Err(_) => return false,
            }
        }
    };
    let (a, b) = tokio::join!(dropped(silent), dropped(slow));
    assert!(a && b, "silent {a}, slow {b}");
    // 15 s for the silent one, 20 s for the headers.
    assert!(start.elapsed() < Duration::from_secs(25), "{:?}", start.elapsed());
}

#[tokio::test]
async fn key_packages_only_of_people_you_share_a_channel_with() {
    let s = server_with(|_| {}).await;
    let (alice, bob) = (login(&s, "alice").await, login(&s, "bob").await);
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    // Bob's device has 30 one-time packages and a last-resort one, as rows:
    // the server hands them out without reading them.
    for i in 0..31 {
        sqlx::query("INSERT INTO e2ee_devices (user_id, device_id, certificate, signature_key, create_at) VALUES (?1, 'bob-phone', '{}', '', 0) ON CONFLICT DO NOTHING")
            .bind(&bob_id)
            .execute(&s.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO e2ee_key_packages (user_id, device_id, data, last_resort, create_at) VALUES (?1, 'bob-phone', ?2, ?3, 0)")
            .bind(&bob_id)
            .bind(vec![i as u8])
            .bind(i == 30)
            .execute(&s.app.db)
            .await
            .unwrap();
    }
    let one_time_left = || async {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM e2ee_key_packages WHERE user_id = ? AND last_resort = 0").bind(&bob_id).fetch_one(&s.app.db).await.unwrap();
        n
    };

    // Not sharing a channel: nothing.
    sqlx::query("DELETE FROM channel_members WHERE user_id = ?").bind(&bob_id).execute(&s.app.db).await.unwrap();
    assert!(alice.claim_key_packages(std::slice::from_ref(&bob_id)).await.unwrap().is_empty());
    assert_eq!(one_time_left().await, 30);

    // Sharing one: twenty one-time packages, then only the last-resort one.
    admin::add_channel(&s.app, "home", "shared", "Shared").await.unwrap();
    for _ in 0..25 {
        let claimed = alice.claim_key_packages(&[bob_id.clone(), bob_id.clone()]).await.unwrap();
        assert_eq!(claimed.len(), 1, "one per device, however often asked");
    }
    assert_eq!(one_time_left().await, 10, "alice took 20");
    let last = alice.claim_key_packages(std::slice::from_ref(&bob_id)).await.unwrap();
    assert_eq!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &last[0].key_package).unwrap(), [30]);

    // Too many claims in a row are refused.
    let mut refused = false;
    for _ in 0..40 {
        if let Err(e) = alice.claim_key_packages(std::slice::from_ref(&alice_id)).await {
            assert!(format!("{e:?}").contains("429"), "{e:?}");
            refused = true;
            break;
        }
    }
    assert!(refused);
}

/// Sends a whole upload of `bytes` as `name`, then hangs up without waiting
/// for the answer.
async fn upload_and_hang_up(s: &Server, c: &Client, channel: &str, name: &str, bytes: &[u8]) {
    let b = "XXboundaryXX";
    let mut body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{channel}\r\n\
         --{b}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    let head = format!(
        "POST /api/v4/files HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Type: multipart/form-data; boundary={b}\r\nContent-Length: {}\r\n\r\n",
        c.token().unwrap(),
        body.len()
    );
    let mut conn = tokio::net::TcpStream::connect(s.url.trim_start_matches("http://")).await.unwrap();
    conn.write_all(head.as_bytes()).await.unwrap();
    conn.write_all(&body).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn uploads_whose_app_hangs_up_are_recorded_and_tidied() {
    let s = server_with(|c| c.uploads_per_hour = 0).await;
    let alice = login(&s, "alice").await;
    let town = town_square(&alice).await;
    let picture = png(2000, 2000);
    for i in 0..8 {
        upload_and_hang_up(&s, &alice, &town, &format!("p{i}.png"), &picture).await;
    }
    // Half an upload, then gone: nothing of it stays on disk.
    let mut half = tokio::net::TcpStream::connect(s.url.trim_start_matches("http://")).await.unwrap();
    let head = format!(
        "POST /api/v4/files HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: 999999\r\n\r\n\
         --B\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{town}\r\n--B\r\nContent-Disposition: form-data; name=\"files\"; filename=\"h.bin\"\r\n\r\n",
        alice.token().unwrap()
    );
    half.write_all(head.as_bytes()).await.unwrap();
    half.write_all(&[7; 50_000]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(half);

    // Each upload that got a turn (three at once: the others were refused)
    // is recorded with its previews, so it's counted and tidied away if
    // never posted. No file is left on disk without a row.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut settled = 0;
    loop {
        let rows: Vec<(String, bool)> = sqlx::query_as("SELECT id, has_preview_image FROM files").fetch_all(&s.app.db).await.unwrap();
        let on_disk = stored_files(&s);
        settled = if rows.len() >= 3 && on_disk == rows.len() * 3 { settled + 1 } else { 0 };
        if settled == 10 {
            assert!(rows.iter().all(|(_, preview)| *preview));
            break;
        }
        assert!(Instant::now() < deadline, "{} rows, {on_disk} files", rows.len());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
