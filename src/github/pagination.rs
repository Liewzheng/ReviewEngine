//! GitHub API pagination support.
//!
//! Provides helpers for following Link header `rel="next"` URLs
//! to fetch all pages of a paginated GitHub REST API response.

use anyhow::{Context, Result};
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

use super::client::Client;

/// Fetch all pages of a paginated list endpoint.
///
/// Follows the `Link` header `rel="next"` pattern. Stops when no
/// next page is found or after `max_pages` (default: 5) to avoid
/// runaway requests in edge cases.
pub async fn get_all_paginated<T>(client: &Client, initial_url: &str, max_pages: u32) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let mut all_items: Vec<T> = Vec::new();
    let mut url = initial_url.to_string();

    for _page in 0..max_pages {
        let (resp, headers) = get_with_headers(client, &url).await?;
        let items: Vec<T> = resp
            .json()
            .await
            .with_context(|| format!("Failed to parse page from {url}"))?;
        let count = items.len();
        all_items.extend(items);

        let next_url = headers.get("Link").and_then(|v| v.to_str().ok()).and_then(|link_str| {
            link_str.split(',').find_map(|part| {
                let part = part.trim();
                if part.ends_with("rel=\"next\"") {
                    let start = part.find('<')?;
                    let end = part.find('>')?;
                    Some(part[start + 1..end].to_string())
                } else {
                    None
                }
            })
        });

        match next_url {
            Some(next) => url = next,
            None => break,
        }

        if count == 0 {
            break;
        }
    }

    Ok(all_items)
}

/// Send a GET request through the client and return the response with headers.
async fn get_with_headers(client: &Client, url: &str) -> Result<(reqwest::Response, HeaderMap)> {
    let resp = client
        .get_http()
        .get(url)
        .headers(client.get_headers())
        .send()
        .await
        .with_context(|| format!("Failed to GET {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API returned {status}: {text}");
    }

    let headers = resp.headers().clone();
    Ok((resp, headers))
}
