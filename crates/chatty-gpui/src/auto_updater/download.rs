//! Update-binary download with progress reporting for the auto-updater.

use super::*;

pub(super) async fn download_update(asset: ReleaseAsset, cx: &mut AsyncApp) {
    cx.update(|cx| {
        cx.update_global::<AutoUpdater, _>(|updater, _cx| {
            updater.status = AutoUpdateStatus::Downloading(0.0);
        });
    })
    .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
    .ok();

    info!(
        url = &asset.download_url,
        version = &asset.version,
        "Starting update download"
    );

    // Create temp file for download
    let temp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            error!(error = ?e, "Failed to create temp directory");
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status =
                        AutoUpdateStatus::Error(format!("Failed to create temp dir: {}", e));
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
            return;
        }
    };

    let download_path = temp_dir.path().join(&asset.name);

    // Download with progress tracking
    match download_file(&asset.download_url, &download_path, cx).await {
        Ok(()) => {
            info!(path = ?download_path, "Download complete");

            // Verify checksum if available
            if let Some(ref expected_hash) = asset.sha256 {
                info!(
                    expected_hash = expected_hash,
                    "Verifying download integrity"
                );

                match AutoUpdater::verify_checksum(&download_path, expected_hash).await {
                    Ok(true) => {
                        info!("Checksum verification passed");
                    }
                    Ok(false) => {
                        error!(
                            expected = expected_hash,
                            "Checksum verification failed - download may be corrupted or tampered"
                        );
                        // Delete the corrupted file
                        let _ = tokio::fs::remove_file(&download_path).await;

                        cx.update(|cx| {
                            cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                updater.status = AutoUpdateStatus::Error(
                                    "Security check failed: Download integrity verification failed. \
                                     The downloaded file does not match the expected checksum."
                                        .to_string(),
                                );
                            });
                        })
                        .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI")).ok();
                        return;
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to verify checksum");
                        // Delete the file to be safe
                        let _ = tokio::fs::remove_file(&download_path).await;

                        cx.update(|cx| {
                            cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                                updater.status = AutoUpdateStatus::Error(format!(
                                    "Checksum verification error: {}",
                                    e
                                ));
                            });
                        })
                        .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                        .ok();
                        return;
                    }
                }
            } else {
                error!(
                    "Security check failed: No checksum available for this release. \
                     Checksums are mandatory for security. This must be fixed in the release process."
                );
                // Delete the downloaded file since we cannot verify its integrity
                let _ = tokio::fs::remove_file(&download_path).await;

                cx.update(|cx| {
                    cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                        updater.status = AutoUpdateStatus::Error(
                            "Security check failed: No checksum available for this release. \
                             Updates require integrity verification."
                                .to_string(),
                        );
                    });
                })
                .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
                .ok();
                return;
            }

            let final_path = download_path.clone();
            let _ = temp_dir.keep(); // Persist temp dir

            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Ready(asset.version.clone(), final_path);
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
        }
        Err(e) => {
            error!(error = ?e, "Download failed");
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Error(format!("Download failed: {}", e));
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
        }
    }
}

/// Download a file with progress tracking
pub(super) async fn download_file(
    url: &str,
    path: &PathBuf,
    cx: &mut AsyncApp,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = chatty_core::services::http_client::default_client(120);
    let response = client.get(url).send().await?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }

    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        // Update progress
        if total_size > 0 {
            let progress = downloaded as f32 / total_size as f32;
            cx.update(|cx| {
                cx.update_global::<AutoUpdater, _>(|updater, _cx| {
                    updater.status = AutoUpdateStatus::Downloading(progress);
                });
            })
            .map_err(|e| warn!(error = ?e, "Failed to update auto-updater UI"))
            .ok();
        }
    }

    file.flush().await?;
    Ok(())
}
