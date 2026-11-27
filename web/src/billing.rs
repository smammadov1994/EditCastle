use anyhow::{Context, Result, ensure};
use reqwest::{Client, Url, header::HeaderValue};
use ring::hmac;
use serde::Deserialize;
use std::{env, time::Duration};
use uuid::Uuid;

pub struct Billing {
    api: Url,
    secret: HeaderValue,
    webhook: hmac::Key,
    pub credits: u32,
    pub amount: u32,
}

#[derive(Deserialize)]
pub struct Checkout {
    pub id: String,
    pub url: String,
    livemode: bool,
}

pub struct Payment {
    pub account: Uuid,
    pub checkout: String,
    pub amount: u32,
}

impl Billing {
    pub fn configured(fixture: bool) -> Result<Option<Self>> {
        let key = env::var("EDITROOM_STRIPE_SECRET_KEY").ok();
        let signing = env::var("EDITROOM_STRIPE_WEBHOOK_SECRET").ok();
        let (key, signing) = match (key, signing) {
            (None, None) => return Ok(None),
            (Some(key), Some(signing)) => (key, signing),
            _ => anyhow::bail!("Set both the Stripe test key and webhook signing secret."),
        };
        ensure!(
            key.starts_with("sk_test_") || key.starts_with("rk_test_"),
            "Use a Stripe test-mode key."
        );
        ensure!(
            signing.starts_with("whsec_") && signing.len() > 12,
            "Set the Stripe webhook signing secret."
        );
        let mut secret = HeaderValue::from_str(&format!("Bearer {key}"))
            .context("The Stripe API key is invalid.")?;
        secret.set_sensitive(true);
        let api = match env::var("EDITROOM_FIXTURE_STRIPE_URL") {
            Ok(value) => {
                let url = Url::parse(&value)?;
                ensure!(
                    fixture && url.scheme() == "http" && url.host_str() == Some("127.0.0.1"),
                    "Stripe fixtures require a local test instance."
                );
                url
            }
            Err(_) => Url::parse("https://api.stripe.com")?,
        };
        let credits = env::var("EDITROOM_STRIPE_CREDITS")
            .unwrap_or_else(|_| "100".into())
            .parse::<u32>()
            .context("Set the render credits in the Stripe test pack.")?;
        let amount = env::var("EDITROOM_STRIPE_PRICE_CENTS")
            .unwrap_or_else(|_| "500".into())
            .parse::<u32>()
            .context("Set the USD price of the Stripe test pack in cents.")?;
        ensure!(
            credits > 0 && amount >= 50,
            "Use a positive credit pack and an amount of at least 50 cents."
        );
        Ok(Some(Self {
            api,
            secret,
            webhook: hmac::Key::new(hmac::HMAC_SHA256, signing.as_bytes()),
            credits,
            amount,
        }))
    }

    pub async fn checkout(&self, client: &Client, account: Uuid, origin: &str) -> Result<Checkout> {
        let fields = [
            ("mode", "payment".to_owned()),
            ("payment_method_types[0]", "card".to_owned()),
            ("client_reference_id", account.to_string()),
            ("line_items[0][quantity]", "1".to_owned()),
            ("line_items[0][price_data][currency]", "usd".to_owned()),
            (
                "line_items[0][price_data][unit_amount]",
                self.amount.to_string(),
            ),
            (
                "line_items[0][price_data][product_data][name]",
                format!("{} render credits", self.credits),
            ),
            ("success_url", format!("{origin}/studio?checkout=returned")),
            ("cancel_url", format!("{origin}/studio?checkout=cancelled")),
        ];
        let body = url_encoded(&fields);
        let checkout: Checkout = client
            .post(self.api.join("/v1/checkout/sessions")?)
            .header(reqwest::header::AUTHORIZATION, &self.secret)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let url = Url::parse(&checkout.url)?;
        ensure!(
            !checkout.livemode
                && valid_checkout(&checkout.id)
                && url.scheme() == "https"
                && url.host_str() == Some("checkout.stripe.com")
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "Stripe did not return a test checkout."
        );
        Ok(checkout)
    }

    pub fn payment(&self, signature: &str, body: &[u8], now: u64) -> Result<Option<Payment>> {
        let mut parts = signature
            .split(',')
            .filter_map(|item| item.trim().split_once('='));
        let (mut timestamp, mut signatures) = (None, Vec::new());
        for (kind, value) in &mut parts {
            match kind {
                "t" => {
                    ensure!(timestamp.is_none(), "Invalid Stripe signature.");
                    timestamp = Some(value);
                }
                "v1" => signatures.push(value),
                _ => {}
            }
        }
        let timestamp = timestamp.context("Missing Stripe timestamp.")?;
        let time = timestamp.parse::<u64>()?;
        ensure!(time.abs_diff(now) <= 300, "Expired Stripe signature.");
        let mut signed = Vec::with_capacity(timestamp.len() + 1 + body.len());
        signed.extend_from_slice(timestamp.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body);
        ensure!(
            signatures
                .into_iter()
                .filter_map(signature_bytes)
                .any(|candidate| hmac::verify(&self.webhook, &signed, &candidate).is_ok()),
            "Invalid Stripe signature."
        );
        let event: serde_json::Value = serde_json::from_slice(body)?;
        if event["livemode"] != false
            || !matches!(
                event["type"].as_str(),
                Some("checkout.session.completed" | "checkout.session.async_payment_succeeded")
            )
        {
            return Ok(None);
        }
        let checkout = &event["data"]["object"];
        if checkout["mode"] != "payment"
            || checkout["payment_status"] != "paid"
            || checkout["currency"] != "usd"
            || checkout["livemode"] != false
        {
            return Ok(None);
        }
        let Some(id) = checkout["id"].as_str().filter(|id| valid_checkout(id)) else {
            return Ok(None);
        };
        let Some(account) = checkout["client_reference_id"]
            .as_str()
            .and_then(|value| value.parse().ok())
        else {
            return Ok(None);
        };
        let Some(amount) = checkout["amount_total"]
            .as_u64()
            .and_then(|amount| u32::try_from(amount).ok())
        else {
            return Ok(None);
        };
        Ok(Some(Payment {
            account,
            checkout: id.into(),
            amount,
        }))
    }
}

fn valid_checkout(value: &str) -> bool {
    value.len() <= 200
        && value.strip_prefix("cs_test_").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn url_encoded(fields: &[(&str, String)]) -> String {
    let mut query = Url::parse("https://api.stripe.com").expect("valid Stripe URL");
    query
        .query_pairs_mut()
        .extend_pairs(fields.iter().map(|(key, value)| (*key, value)));
    query.query().unwrap_or_default().to_owned()
}

fn signature_bytes(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.is_ascii() {
        return None;
    }
    let mut output = [0; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(output)
}
