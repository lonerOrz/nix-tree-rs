use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Stdio;
use tokio::process::Command;

use crate::store_path::{StorePath, StorePathGraph};

#[derive(Debug, Deserialize)]
struct NixPathInfo {
    #[serde(rename = "narHash")]
    #[allow(dead_code)]
    nar_hash: Option<String>,

    #[serde(rename = "narSize")]
    nar_size: Option<u64>,

    #[serde(rename = "path")]
    path: String,

    references: Option<Vec<String>>,
    signatures: Option<Vec<String>>,

    #[serde(rename = "closureSize")]
    closure_size: Option<u64>,
}

async fn resolve_paths(
    paths: &[String],
    store: Option<&str>,
    nix_options: &[(String, String)],
    file: Option<&str>,
) -> Result<Vec<String>> {
    let mut cmd = Command::new("nix");
    cmd.arg("--extra-experimental-features")
        .arg("nix-command flakes");

    for (name, value) in nix_options {
        cmd.arg("--option").arg(name).arg(value);
    }

    if let Some(store_url) = store {
        cmd.arg("--store").arg(store_url);
    }

    if let Some(file_path) = file {
        cmd.arg("--file").arg(file_path);
    }

    cmd.arg("path-info")
        .arg("--json")
        .args(paths)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().await.context("Failed to run nix path-info")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix path-info failed: {}", stderr);
    }

    let json_str = String::from_utf8(output.stdout).context("Invalid UTF-8 in nix output")?;
    let path_info_map: std::collections::HashMap<String, NixPathInfo> =
        serde_json::from_str(&json_str).or_else(|_| {
            let list: Vec<NixPathInfo> = serde_json::from_str(&json_str)?;
            let map: std::collections::HashMap<String, NixPathInfo> = list
                .into_iter()
                .map(|info| (info.path.clone(), info))
                .collect();
            Ok::<_, anyhow::Error>(map)
        })?;

    Ok(path_info_map.keys().cloned().collect())
}

pub async fn query_path_info(
    paths: &[String],
    recursive: bool,
    store: Option<&str>,
    nix_options: &[(String, String)],
    file: Option<&str>,
) -> Result<StorePathGraph> {
    let resolved_paths = resolve_paths(paths, store, nix_options, file).await?;

    for path in &resolved_paths {
        if !std::path::Path::new(path).exists() {
            let status = std::process::Command::new("nix-store")
                .arg("--realise")
                .arg(path)
                .status()
                .context("Failed to run nix-store --realise")?;

            if !status.success() {
                anyhow::bail!("Failed to realise store path: {}", path);
            }
        }
    }

    let mut cmd = Command::new("nix");
    cmd.arg("--extra-experimental-features")
        .arg("nix-command flakes");

    for (name, value) in nix_options {
        cmd.arg("--option").arg(name).arg(value);
    }

    if let Some(store_url) = store {
        cmd.arg("--store").arg(store_url);
    }

    if let Some(file_path) = file {
        cmd.arg("--file").arg(file_path);
    }

    cmd.arg("path-info")
        .arg("--json")
        .arg("--closure-size")
        .args(&resolved_paths)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if recursive {
        cmd.arg("--recursive");
    }

    let output = cmd
        .output()
        .await
        .context("Failed to execute nix path-info")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix path-info failed: {}", stderr);
    }

    let json_str = String::from_utf8(output.stdout).context("Invalid UTF-8 in nix output")?;

    let path_info_map: std::collections::HashMap<String, NixPathInfo> =
        serde_json::from_str(&json_str).or_else(|_| {
            let list: Vec<NixPathInfo> = serde_json::from_str(&json_str)?;
            let map: std::collections::HashMap<String, NixPathInfo> = list
                .into_iter()
                .map(|info| (info.path.clone(), info))
                .collect();
            Ok::<_, anyhow::Error>(map)
        })?;

    let mut graph = StorePathGraph::new();

    for (path, info) in path_info_map {
        let (hash, name) = StorePath::parse(&path)?;

        let store_path = StorePath {
            path: path.clone(),
            hash,
            name,
            nar_size: info.nar_size.unwrap_or(0),
            closure_size: info.closure_size,
            references: info.references.unwrap_or_default(),
            signatures: info.signatures.unwrap_or_default(),
        };

        graph.add_path(store_path);
    }

    graph.roots = resolved_paths;
    graph.disambiguate_names();

    Ok(graph)
}

pub async fn get_default_roots() -> Result<Vec<String>> {
    let mut roots = Vec::new();

    let system_profile = "/nix/var/nix/profiles/system";
    if tokio::fs::metadata(system_profile).await.is_ok() {
        roots.push(system_profile.to_string());
    }

    if let Ok(user) = std::env::var("USER") {
        let user_profile = format!("/nix/var/nix/profiles/per-user/{user}/profile");
        if tokio::fs::metadata(&user_profile).await.is_ok() {
            roots.push(user_profile);
        }
    }

    if roots.is_empty() {
        anyhow::bail!("No default roots found. Please specify a path.");
    }

    Ok(roots)
}
