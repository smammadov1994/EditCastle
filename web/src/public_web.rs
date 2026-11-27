use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Response, Url, header};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use std::{collections::HashSet, net::IpAddr, time::Duration};

pub fn address(text: &str) -> Result<Url> {
    let url = Url::parse(text.trim()).context("Enter a valid public HTTPS URL.")?;
    ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.domain().is_some(),
        "Use a public HTTPS hostname."
    );
    Ok(url)
}

fn usable_ip(address: IpAddr) -> bool {
    // Pin to checked native IPv4 addresses to prevent DNS rebinding and IPv6 transition aliases.
    let IpAddr::V4(address) = address else {
        return false;
    };
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || a == 169 && b == 254
        || a == 172 && (16..=31).contains(&b)
        || a == 192 && b == 168
        || a == 100 && (64..=127).contains(&b)
        || a == 192 && (b == 0 || b == 88 && c == 99)
        || a == 198 && (b == 18 || b == 19 || b == 51 && c == 100)
        || a == 203 && b == 0 && c == 113)
}

pub async fn fetch(url: &str) -> Result<Response> {
    let mut url = address(url)?;
    for _ in 0..5 {
        let host = url
            .domain()
            .context("Use a public HTTPS hostname.")?
            .to_owned();
        let addresses = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::lookup_host((host.as_str(), 443)),
        )
        .await??
        .filter(|address| usable_ip(address.ip()))
        .collect::<Vec<_>>();
        ensure!(
            !addresses.is_empty(),
            "Only public websites can be reached."
        );

        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addresses)
            .user_agent("EditCastle/0.1 (video editor; media import)")
            .timeout(Duration::from_secs(60))
            .build()?;
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Could not reach the website."))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .context("Invalid website redirect.")?;
            url = address(
                url.join(location)
                    .context("Invalid website redirect.")?
                    .as_str(),
            )?;
            continue;
        }
        ensure!(
            response.status().is_success(),
            "The website returned HTTP {}.",
            response.status().as_u16()
        );
        return Ok(response);
    }
    bail!("Too many website redirects.")
}

pub async fn read_page(url: &str) -> Result<Value> {
    const LIMIT: usize = 2 * 1024 * 1024;
    let mut response = fetch(url).await?;
    ensure!(
        response.content_length().unwrap_or(0) <= LIMIT as u64,
        "The page is too large to read."
    );
    let homepage = response.url().clone();
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ensure!(
        matches!(
            mime.as_str(),
            "text/html" | "application/xhtml+xml" | "text/plain" | "text/markdown"
        ),
        "Use an HTML or plain-text page."
    );
    let mut data = Vec::new();
    while let Some(bytes) = response.chunk().await? {
        ensure!(
            data.len() + bytes.len() <= LIMIT,
            "The page is too large to read."
        );
        data.extend_from_slice(&bytes);
    }
    let text = String::from_utf8_lossy(&data);
    Ok(
        if matches!(mime.as_str(), "text/html" | "application/xhtml+xml") {
            parse_page(&homepage, &text)
        } else {
            json!({"url":homepage.as_str(), "text":text.chars().take(20000).collect::<String>()})
        },
    )
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("fixed HTML selector")
}

fn parse_page(url: &Url, html: &str) -> Value {
    let document = Html::parse_document(html);
    let title = document
        .select(&selector("title"))
        .next()
        .map(|part| part.text().collect::<String>())
        .unwrap_or_default();
    let body = document
        .select(&selector("main,article,[role=main]"))
        .next()
        .or_else(|| document.select(&selector("body")).next());
    let mut text = String::new();
    if let Some(body) = body {
        for node in body.descendants() {
            let Some(fragment) = node.value().as_text() else {
                continue;
            };
            if node
                .ancestors()
                .take_while(|parent| *parent != *body)
                .filter_map(ElementRef::wrap)
                .any(|parent| {
                    matches!(
                        parent.value().name(),
                        "script" | "style" | "nav" | "svg" | "template" | "noscript"
                    )
                })
            {
                continue;
            }
            let trimmed = fragment.trim();
            if !trimmed.is_empty() {
                text.push_str(trimmed);
                text.push(' ');
            }
        }
    }
    let mut seen = HashSet::new();
    let mut links = document.select(&selector("a[href], audio[src], audio source[src], link[rel=license]"))
        .filter_map(|item| {
            let raw = item.attr("href").or_else(|| item.attr("src"))?;
            let mut target = url.join(raw).ok()?;
            target.set_fragment(None);
            let target = address(target.as_str()).ok()?;
            if target == *url || !seen.insert(target.to_string()) { return None; }
            let label = item.text().collect::<String>();
            let licence = item.attr("rel") == Some("license")
                || target.domain() == Some("creativecommons.org")
                || label.to_lowercase().contains("licen");
            let audio = item.attr("src").is_some()
                || [".ogg",".oga",".mp3",".wav",".m4a",".flac",".opus",".aac"]
                    .iter().any(|suffix| target.path().to_ascii_lowercase().ends_with(suffix));
            let explicit = item.attr("download").is_some();
            Some((!(licence || audio || explicit), json!({
                "url": target.as_str(), "label": label.trim().chars().take(200).collect::<String>(),
            })))
        }).collect::<Vec<_>>();
    links.sort_by_key(|(ordinary, _)| *ordinary);
    json!({"url":url.as_str(), "title":title.trim().chars().take(240).collect::<String>(),
        "text":text.chars().take(20000).collect::<String>(),
        "links":links.into_iter().take(45).map(|(_, link)| link).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_page_returns_text_and_direct_audio_link() {
        let url = address("https://music.example/track").unwrap();
        let page = parse_page(
            &url,
            r#"<title>Moonlight</title>
            <main><p>CC0 &amp; usable commercially</p>
            <script>Private embedded script</script>
            <a href="/other">Other</a>
            <a href="/track.wav" download>Get the audio</a>
            <a href="https://creativecommons.org/publicdomain/zero/1.0/" rel="license">Licence</a>
            <a href="javascript:void(0)">Bad link</a></main>"#,
        );
        assert_eq!(page["title"], "Moonlight");
        assert!(
            page["text"]
                .as_str()
                .unwrap()
                .contains("CC0 & usable commercially")
        );
        assert!(
            !page["text"]
                .as_str()
                .unwrap()
                .contains("Private embedded script")
        );
        assert_eq!(page["links"][0]["url"], "https://music.example/track.wav");
        assert_eq!(page["links"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn public_fetch_rejects_private_addresses_and_non_https_urls() {
        for raw in [
            "https://127.0.0.1/a",
            "https://[::1]/a",
            "http://example.com/a",
            "https://user:password@example.com/a",
            "https://example.com:48188/a",
        ] {
            assert!(address(raw).is_err(), "{raw}");
        }
        for raw in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.100.100.200",
            "::ffff:127.0.0.1",
        ] {
            assert!(!usable_ip(raw.parse().unwrap()), "{raw}");
        }
    }
}
