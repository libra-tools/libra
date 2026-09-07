//! Integration tests for `libra media` — the feature-gated FastCDC LFS media
//! chunking client (lore.md §6). Compiled only under `--features fastcdc`.
//!
//! Covers local chunk/store/verify, bounded HTTP transfers against loopback
//! fixtures, integrity failures, and ordinary LFS fallback. An ignored test
//! connects the real Libra client to Mega's production media router.
//! Layer: L1 by default (temporary directories and local loopback only).
#![cfg(feature = "fastcdc")]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn media_bin() -> &'static str {
    env!("CARGO_BIN_EXE_libra")
}

fn supported_capabilities(fallback: bool) -> serde_json::Value {
    serde_json::json!({
        "version": "1", "chunked_lfs": true,
        "chunk_algorithms": ["fastcdc-v1"], "hash_algorithms": ["sha256"],
        "max_chunk_size": 8 * 1024 * 1024, "max_manifest_size": 10 * 1024 * 1024,
        "supports_batch_exists": true, "supports_range_read": false,
        "supports_standard_lfs_fallback": fallback
    })
}

#[tokio::test]
async fn invalid_remote_manifest_or_chunk_preserves_existing_destination() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Json, Router, routing::get};
    use libra::utils::media::{
        chunk_store::MediaChunkStore, manifest::MediaManifest, transfer::MediaClient,
    };

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::write(&source, b"correct media").unwrap();
    let (manifest, _) = MediaManifest::build_from_file(&source).unwrap();
    for wrong_identity in [false, true] {
        let id = if wrong_identity {
            "a".repeat(64)
        } else {
            manifest.id().unwrap()
        };
        let returned = serde_json::json!({"manifest_id": id, "manifest": manifest});
        let chunk_requests = Arc::new(AtomicUsize::new(0));
        let counted = chunk_requests.clone();
        let app = Router::new()
            .route(
                "/repo.git/info/lfs/libra/media/v1/capabilities",
                get(|| async { Json(supported_capabilities(true)) }),
            )
            .route(
                "/repo.git/info/lfs/libra/media/v1/manifests/by-media/{oid}",
                get(move || {
                    let response = returned.clone();
                    async move { Json(response) }
                }),
            )
            .route(
                "/repo.git/info/lfs/libra/media/v1/manifests/by-media/{oid}/chunks/{hash}",
                get(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    async { "corrupt media" }
                }),
            )
            .layer(axum::middleware::from_fn(
                |request: axum::extract::Request, next: axum::middleware::Next| async move {
                    assert_eq!(request.uri().query(), Some("tenant=test"));
                    next.run(request).await
                },
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!(
            "http://{}/repo.git/info/lfs/?tenant=test",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = MediaClient::discover(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            &base,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        let dest = dir.path().join("dest");
        fs::write(&dest, b"keep me").unwrap();
        let store = MediaChunkStore::at(dir.path().join("chunks"));
        let error = client
            .download(&manifest.media_oid, manifest.media_size, &dest, &store)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(if wrong_identity {
                "manifest does not match"
            } else {
                "chunk size or SHA-256 mismatch"
            }),
            "{error:#}"
        );
        assert_eq!(
            chunk_requests.load(Ordering::SeqCst),
            usize::from(!wrong_identity)
        );
        assert_eq!(fs::read(dest).unwrap(), b"keep me");
        task.abort();
    }
}

#[tokio::test]
async fn ordinary_lfs_server_falls_back_to_full_transfer() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use axum::{
        Json, Router,
        body::Bytes,
        routing::{get, post, put},
    };
    use libra::{internal::protocol::lfs_client::LFSClient, utils::media::transfer::MediaClient};

    // No extension, chunk-only policy, and an extension whose manifest limit
    // is too small must all retain the standard complete-object path.
    for (advertise, fallback, small_manifest) in [
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let data = "plain LFS bytes";
        let oid =
            hex::encode(ring::digest::digest(&ring::digest::SHA256, data.as_bytes()).as_ref());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = format!("http://{}/repo.git", listener.local_addr().unwrap());
        let base = format!("{remote}/info/lfs/");
        let object_base = format!("{base}objects/");
        let uploaded = Arc::new(AtomicBool::new(false));
        let saved = uploaded.clone();
        let mut app = Router::new()
            .route("/repo.git/info/lfs/objects/batch", post(move |Json(request): Json<serde_json::Value>| {
                let object_base = object_base.clone();
                async move {
                    let action = request["operation"].as_str().unwrap();
                    let requested_oid = request["objects"][0]["oid"].as_str().unwrap();
                    let href = format!("{object_base}{requested_oid}");
                    Json(serde_json::json!({"transfer":"basic","objects":[{
                        "oid":requested_oid, "size":data.len(), "actions":{(action):{"href":href,"expires_at":""}}
                    }]}))
                }
            }))
            .route("/repo.git/info/lfs/objects/{oid}", get(move || async move { data }).merge(put(move |body: Bytes| {
                assert_eq!(body.as_ref(), data.as_bytes());
                saved.store(true, Ordering::SeqCst);
                async { axum::http::StatusCode::OK }
            })));
        if advertise {
            app = app.route(
                "/repo.git/info/lfs/libra/media/v1/capabilities",
                get(move || async move {
                    let mut caps = supported_capabilities(fallback);
                    if small_manifest {
                        caps["max_manifest_size"] = serde_json::json!(32);
                    }
                    Json(caps)
                }),
            );
        }
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut client = LFSClient::from_remote_url(&remote).unwrap();
        client.client = reqwest::Client::builder().no_proxy().build().unwrap();
        // A chunk-only advertisement must not block either basic operation.
        assert_eq!(
            MediaClient::discover(client.client.clone(), &client.lfs_url, false)
                .await
                .unwrap()
                .is_some(),
            small_manifest,
        );
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, data).unwrap();
        assert!(client.push_object(&oid, &source).await.unwrap());
        assert!(uploaded.load(Ordering::SeqCst));
        let dest = dir.path().join("dest");
        client
            .download_object(&oid, data.len() as u64, &dest, None)
            .await
            .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data.as_bytes());
        // A legal SHA-256 OID for different content reaches the checksum error
        // path. Its replacement pointer must be visible as soon as we return.
        let wrong_oid = hex::encode(
            ring::digest::digest(&ring::digest::SHA256, b"a different LFS object").as_ref(),
        );
        let error = client
            .download_object(&wrong_oid, data.len() as u64, &dest, None)
            .await
            .expect_err("incorrect object bytes must fail checksum verification");
        assert!(error.to_string().contains("Checksum mismatch"), "{error:#}");
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            libra::utils::lfs::format_pointer_string(&wrong_oid, data.len() as u64),
            "the checksum error must flush the complete fallback pointer before returning"
        );
        task.abort();
    }
}

#[test]
fn relative_reassembly_target_is_replaced_only_after_verification() {
    use libra::utils::media::{
        chunk_store::{self, MediaChunkStore},
        manifest::MediaManifest,
    };
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::write(&source, b"verified replacement").unwrap();
    let (mut manifest, _) = MediaManifest::build_from_file(&source).unwrap();
    let store = MediaChunkStore::at(dir.path().join("chunks"));
    store.put_chunk(b"verified replacement").unwrap();
    // Use an actual leaf path without changing the process working directory.
    let target = tempfile::NamedTempFile::new_in(".")
        .unwrap()
        .into_temp_path();
    let relative = Path::new(target.file_name().unwrap());
    fs::write(relative, b"old contents").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(relative, fs::Permissions::from_mode(0o750)).unwrap();
    }
    chunk_store::reassemble(&manifest, &store, relative).unwrap();
    assert_eq!(fs::read(relative).unwrap(), b"verified replacement");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(relative).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
    manifest.media_oid = "a".repeat(64);
    assert!(chunk_store::reassemble(&manifest, &store, relative).is_err());
    assert_eq!(fs::read(relative).unwrap(), b"verified replacement");
}

/// Run against Mega's real production media router (isolated test database).
/// See Mega docs/lfs-api.md for the two-process invocation.
#[tokio::test]
#[ignore = "requires Mega serve_libra_interop and MEGA_FASTCDC_READY_FILE"]
#[serial_test::serial(cwd)]
async fn mega_fastcdc_http_interop() {
    use libra::{
        internal::protocol::lfs_client::LFSClient,
        utils::{
            media::{chunk_store::MediaChunkStore, manifest::MediaManifest, transfer::MediaClient},
            test::ChangeDirGuard,
        },
    };
    use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

    let ready = std::env::var("MEGA_FASTCDC_READY_FILE").expect("MEGA_FASTCDC_READY_FILE required");
    let connection: serde_json::Value = serde_json::from_slice(&fs::read(ready).unwrap()).unwrap();
    let lfs_url = url::Url::parse(connection["lfs_url"].as_str().unwrap()).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", connection["token"].as_str().unwrap()))
            .unwrap(),
    );
    let http = reqwest::Client::builder()
        .no_proxy()
        .default_headers(headers)
        .build()
        .unwrap();
    let media = MediaClient::discover(http.clone(), &lfs_url, true)
        .await
        .unwrap()
        .expect("Mega must negotiate FastCDC");
    let dir = tempfile::tempdir().unwrap();
    ok(&["init"], dir.path());
    let _cwd = ChangeDirGuard::new(dir.path());
    let remote = lfs_url.as_str().strip_suffix("/info/lfs/").unwrap();
    let mut lfs = LFSClient::from_remote_url(remote).unwrap();
    lfs.client = http.clone();
    let source = dir.path().join("source.bin");
    let mut seed = 0x1234_5678_9abc_def0u64;
    let data: Vec<u8> = (0..12 * 1024 * 1024)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        })
        .collect();
    fs::write(&source, &data).unwrap();
    let (manifest, _) = MediaManifest::build_from_file(&source).unwrap();
    assert!(
        manifest
            .chunks
            .windows(2)
            .any(|c| c[0].length != c[1].length)
    );
    let base = lfs_url.join("libra/media/v1/").unwrap();
    let prepared: serde_json::Value = http
        .post(base.join("manifests").unwrap())
        .json(&manifest)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = prepared["manifest_id"].as_str().unwrap();
    let chunk = &manifest.chunks[0];
    http.put(
        base.join(&format!("manifests/{id}/chunks/{}", chunk.chunk_hash))
            .unwrap(),
    )
    .body(data[..chunk.length as usize].to_vec())
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    // A restart resumes from the server's persisted missing-chunk response.
    assert!(lfs.push_object(&manifest.media_oid, &source).await.unwrap());
    let published: serde_json::Value = http
        .get(
            base.join(&format!("manifests/by-media/{}", manifest.media_oid))
                .unwrap(),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(published["manifest_id"], manifest.id().unwrap());
    let dedup: serde_json::Value = http
        .post(base.join("manifests").unwrap())
        .json(&manifest)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(dedup["missing_chunks"].as_array().unwrap().is_empty());
    assert!(lfs.push_object(&manifest.media_oid, &source).await.unwrap());
    let cache_root = dir.path().join(".libra/media/chunks");
    let store = MediaChunkStore::at(cache_root.clone());
    // Simulate a previously downloaded first chunk, then reconstruct the rest.
    store.put_chunk(&data[..chunk.length as usize]).unwrap();
    let output = dir.path().join("download.bin");
    fs::write(&output, b"previous contents").unwrap();
    lfs.download_object(&manifest.media_oid, manifest.media_size, &output, None)
        .await
        .unwrap();
    assert_eq!(fs::read(&output).unwrap(), data);
    assert!(
        dir.path()
            .join(".libra/media/manifests")
            .join(format!("{}.json", manifest.media_oid))
            .exists(),
        "ordinary LFS download must select FastCDC and persist its manifest"
    );
    let cached = cache_root
        .join(&chunk.chunk_hash[..2])
        .join(&chunk.chunk_hash[2..]);
    fs::write(cached, b"corrupt cache").unwrap();
    lfs.download_object(&manifest.media_oid, manifest.media_size, &output, None)
        .await
        .unwrap();
    assert_eq!(fs::read(&output).unwrap(), data);
    let bob = reqwest::Client::builder()
        .no_proxy()
        .default_headers(HeaderMap::from_iter([(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer test-bob"),
        )]))
        .build()
        .unwrap();
    let mut bob_lfs = LFSClient::from_remote_url(remote).unwrap();
    bob_lfs.client = bob.clone();
    let bob = MediaClient::discover(bob, &lfs_url, false)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !bob.download(&manifest.media_oid, manifest.media_size, &output, &store)
            .await
            .unwrap()
    );
    bob_lfs
        .download_object(&manifest.media_oid, manifest.media_size, &output, None)
        .await
        .unwrap();
    assert_eq!(
        fs::read(&output).unwrap(),
        data,
        "other users retain complete standard LFS access"
    );

    // Reproduce the finalize crash window: a complete basic object is present,
    // but this user's manifest has not been published. Batch now omits upload
    // actions; the normal push path must still repair the missing manifest.
    let recover = dir.path().join("recover.bin");
    fs::write(&recover, b"complete fallback without a manifest").unwrap();
    let (recover_manifest, _) = MediaManifest::build_from_file(&recover).unwrap();
    let batch: serde_json::Value = http
        .post(lfs.batch_url.clone())
        .json(&serde_json::json!({
            "operation":"upload", "transfers":["basic"], "hash_algo":"sha256",
            "objects":[{"oid":recover_manifest.media_oid,"size":recover_manifest.media_size}]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    http.put(
        batch["objects"][0]["actions"]["upload"]["href"]
            .as_str()
            .unwrap(),
    )
    .body(fs::read(&recover).unwrap())
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    assert_eq!(
        http.get(
            base.join(&format!(
                "manifests/by-media/{}",
                recover_manifest.media_oid
            ))
            .unwrap()
        )
        .send()
        .await
        .unwrap()
        .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert!(
        lfs.push_object(&recover_manifest.media_oid, &recover)
            .await
            .unwrap()
    );
    http.get(
        base.join(&format!(
            "manifests/by-media/{}",
            recover_manifest.media_oid
        ))
        .unwrap(),
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    let empty = dir.path().join("empty.bin");
    fs::write(&empty, []).unwrap();
    let (manifest, _) = MediaManifest::build_from_file(&empty).unwrap();
    assert!(lfs.push_object(&manifest.media_oid, &empty).await.unwrap());
    assert!(
        media
            .download(&manifest.media_oid, 0, &output, &store)
            .await
            .unwrap()
    );
    assert!(fs::read(&output).unwrap().is_empty());
}

fn run(args: &[&str], cwd: &Path) -> Output {
    let home = cwd.join(".libra-test-home");
    fs::create_dir_all(home.join(".config")).unwrap();
    Command::new(media_bin())
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(
            "LIBRA_CONFIG_GLOBAL_DB",
            home.join(".libra").join("config.db"),
        )
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("run libra")
}

fn ok(args: &[&str], cwd: &Path) -> Output {
    let out = run(args, cwd);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// A fresh initialized repo with a media file large enough to split into
/// several chunks (so dedup/reassembly is meaningfully exercised).
fn repo_with_media() -> (tempfile::TempDir, String) {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    ok(&["init"], p);
    // ~5 MiB of pseudo-random-but-fixed bytes → multiple content-defined chunks.
    let mut data = Vec::with_capacity(5 * 1024 * 1024);
    let mut x: u64 = 0x0BADC0DE_DEADBEEF;
    while data.len() < 5 * 1024 * 1024 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        data.push((x >> 32) as u8);
    }
    fs::write(p.join("big.bin"), &data).unwrap();
    (repo, "big.bin".to_string())
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("json stdout")
}

#[test]
fn chunk_store_verify_roundtrip() {
    let (repo, file) = repo_with_media();
    let p = repo.path();

    let out = ok(&["--json", "media", "chunk", &file, "--store"], p);
    let js = json(&out);
    let media_oid = js["data"]["media_oid"].as_str().unwrap().to_string();
    assert_eq!(media_oid.len(), 64, "media_oid is sha256 hex");
    assert!(
        js["data"]["chunk_count"].as_u64().unwrap() > 1,
        "multi-chunk"
    );
    assert_eq!(js["data"]["algorithm"].as_str(), Some("fastcdc-v1"));

    // Manifest + chunk store landed under a private .libra/media sibling of objects/.
    let manifest = p
        .join(".libra")
        .join("media")
        .join("manifests")
        .join(format!("{media_oid}.json"));
    assert!(manifest.exists(), "manifest file persisted");
    assert!(
        p.join(".libra").join("media").join("chunks").exists(),
        "chunk store dir exists"
    );
    // Chunks are NOT in the Git object graph.
    assert!(
        !p.join(".libra").join("objects").join("media").exists(),
        "media must not live under objects/"
    );

    // Reassemble + verify the full media_oid.
    let vout = ok(&["--json", "media", "verify", &file], p);
    assert_eq!(json(&vout)["data"]["verified"].as_bool(), Some(true));

    // Inspect the manifest.
    let iout = ok(
        &["--json", "media", "inspect", manifest.to_str().unwrap()],
        p,
    );
    assert_eq!(
        json(&iout)["data"]["media_oid"].as_str(),
        Some(media_oid.as_str())
    );
    assert_eq!(
        json(&iout)["data"]["hash_algorithm"].as_str(),
        Some("sha256")
    );
}

#[test]
fn verify_fails_cleanly_on_a_corrupt_chunk() {
    let (repo, file) = repo_with_media();
    let p = repo.path();
    ok(&["media", "chunk", &file, "--store"], p);

    // Corrupt one stored chunk by truncating it.
    let chunks_dir = p.join(".libra").join("media").join("chunks");
    let mut a_chunk = None;
    for shard in fs::read_dir(&chunks_dir).unwrap() {
        let shard = shard.unwrap().path();
        if shard.is_dir()
            && let Some(entry) = fs::read_dir(&shard).unwrap().next()
        {
            a_chunk = Some(entry.unwrap().path());
            break;
        }
    }
    fs::write(a_chunk.expect("a stored chunk"), b"tampered").unwrap();

    // Verify now fails (non-zero) — the corrupt chunk is caught on read, and no
    // reassembled output is produced.
    let out = run(&["media", "verify", &file], p);
    assert_ne!(
        out.status.code(),
        Some(0),
        "verify must fail on a corrupt chunk"
    );
}

#[test]
fn probe_unreachable_endpoint_falls_back_to_standard_lfs() {
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    ok(&["init"], p);
    // A refused loopback port → immediate no-endpoint, no external network.
    ok(
        &["config", "remote.origin.url", "https://127.0.0.1:1/x.git"],
        p,
    );

    let out = ok(&["--json", "media", "probe", "--remote", "origin"], p);
    let js = json(&out);
    assert_eq!(
        js["data"]["chunked"].as_bool(),
        Some(false),
        "must fall back"
    );
    assert_eq!(
        js["data"]["decision"].as_str(),
        Some("standard-lfs (fallback)")
    );
    assert_eq!(
        js["data"]["reason"].as_str(),
        Some("no-capability-endpoint")
    );
}
