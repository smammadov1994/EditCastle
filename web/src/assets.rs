use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
    response::Response,
};

macro_rules! files {
    ($root:literal, $($name:literal),+ $(,)?) => {
        &[$(($name, include_bytes!(concat!($root, $name)))),+]
    };
}

pub const LANDING: &[(&str, &[u8])] = files!(
    "../static/landing/",
    "type/barlow-condensed-700.woff2",
    "type/barlow-400.woff2",
    "type/barlow-600.woff2",
    "type/barlowcondensed-OFL.txt",
    "type/barlow-OFL.txt",
    "moments/LICENSE.txt",
    "moments/lib.js",
    "moments/moments.js",
    "moments/liquid-hover/moment.css",
    "moments/liquid-hover/moment.js",
);

pub const STUDIO: &[(&str, &[u8])] = files!(
    "../static/",
    "studio-demo.jpg",
    "studio-reel.jpg",
    "studio-reel.mp4",
);

pub const HOME: &[(&str, &[u8])] = files!(
    "../static/landing-media/",
    "studio-feature.jpg",
    "studio-logo.png",
    "studio-shot-flight.jpg",
    "studio-shot-rise.jpg",
    "studio-shot-sunrise.jpg",
    "reel-flying-turtle.mp4",
    "reel-kite-dragon.jpg",
    "reel-kite-dragon.mp4",
    "reel-paper-boat.jpg",
    "reel-paper-boat.mp4",
    "film-safari.jpg",
    "film-safari.mp4",
    "film-pixel-ghost.jpg",
    "film-pixel-ghost.mp4",
    "film-cowhide.jpg",
    "film-cowhide.mp4",
    "reel-flying-turtle.jpg",
    "safari-shot-1.jpg",
    "safari-shot-2.jpg",
    "safari-shot-3.jpg",
    "safari-shot-4.jpg",
    "safari-shot-5.jpg",
    "safari-shot-6.jpg",
    "safari-shot-7.jpg",
    "wall-safari.jpg",
    "wall-safari.mp4",
    "wall-pixel-ghost.jpg",
    "wall-pixel-ghost.mp4",
    "wall-cowhide.jpg",
    "wall-cowhide.mp4",
    "wall-turtle.jpg",
    "wall-turtle.mp4",
    "wall-dragon.jpg",
    "wall-dragon.mp4",
);

pub fn find(files: &'static [(&str, &[u8])], name: &str) -> Option<&'static [u8]> {
    files
        .iter()
        .find(|(path, _)| *path == name)
        .map(|(_, data)| *data)
}

pub fn serve(request: &Request, name: &str, data: &'static [u8]) -> Response {
    let content_type = match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("jpg") => "image/jpeg",
        Some("png") => "image/png",
        Some("mp4") => "video/mp4",
        _ => "application/octet-stream",
    };
    let mut response = axum::http::Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, content_type);
    let mut bytes = data;
    if request.method() == Method::GET
        && !request.headers().contains_key(header::IF_RANGE)
        && let Some(range) = request.headers().get(header::RANGE)
    {
        let offset = range
            .to_str()
            .ok()
            .and_then(|range| http_range_header::parse_range_header(range).ok())
            .and_then(|ranges| ranges.validate(data.len() as u64).ok());
        let Some([range]) = offset.as_deref() else {
            return response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", data.len()))
                .body(Body::empty())
                .expect("static asset response");
        };
        bytes = &data[*range.start() as usize..=*range.end() as usize];
        response = response.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", range.start(), range.end(), data.len()),
        );
    }
    response
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(bytes)
        })
        .expect("static asset response")
}
