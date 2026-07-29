#![allow(dead_code)]

use globustut_uploader_to_commons::config::Config;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

pub fn test_config(root: &Path) -> Config {
    Config {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        data_dir: root.join("data"),
        upload_dir: root.join("data/pending"),
        thumbnail_dir: root.join("data/thumbnails"),
        temp_dir: root.join("tmp"),
        database_path: root.join("data/test.sqlite3"),
        max_file_bytes: 10 * 1024 * 1024,
        max_request_bytes: 30 * 1024 * 1024,
        max_files: 20,
        disk_reserve_bytes: 0,
        max_pending_bytes: 30 * 1024 * 1024,
        max_pixels: 10_000_000,
        conversion_timeout_seconds: 30,
        request_timeout_seconds: 60,
        session_ttl_seconds: 12 * 60 * 60,
        cookie_secure: false,
        public_base_url: "http://127.0.0.1".into(),
        commons_api_url: "https://commons.wikimedia.org/w/api.php".into(),
        commons_api_timeout_seconds: 30,
        commons_bot_password_path: root.join("data/commons-bot-password"),
        wikidata_sparql_url: "https://query.wikidata.org/sparql".into(),
        nearby_radius_meters: 10_000,
        nearby_limit: 20,
        user_agent: "globustut-uploader-tests/0.1".into(),
    }
}

pub fn run(command: &mut std::process::Command) {
    let output = command.output().expect("failed to start fixture command");
    assert!(
        output.status.success(),
        "fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn imagemagick() -> std::process::Command {
    let program = std::process::Command::new("magick")
        .arg("-version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|_| "magick")
        .unwrap_or("convert");
    std::process::Command::new(program)
}
