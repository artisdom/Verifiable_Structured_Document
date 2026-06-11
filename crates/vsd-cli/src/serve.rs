//! `vsd serve` — the reference object-store server
//! (docs/OBJECT-STORE-HTTP.md, ROADMAP 5e).
//!
//! Deliberately boring: it serves immutable bytes by content address
//! and does nothing else. No rendering, no mutation, no negotiation —
//! every interesting property lives in the *client's* verification.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use vsd_core::ObjectId;

/// Everything the server knows: object bytes by id, container bytes by
/// document id. Built once at startup; immutable thereafter (matching
/// the protocol's own model).
pub struct Index {
    pub objects: BTreeMap<ObjectId, Vec<u8>>,
    pub documents: BTreeMap<ObjectId, Vec<u8>>,
}

pub fn build_index(root: &Path) -> Result<Index> {
    let mut index = Index {
        objects: BTreeMap::new(),
        documents: BTreeMap::new(),
    };
    let mut files = Vec::new();
    collect_vsd_files(root, &mut files)?;
    for path in files {
        let bytes = std::fs::read(&path)?;
        let file = match vsd_container::read_document(&bytes, &Default::default()) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("  skipping {} ({e})", path.display());
                continue;
            }
        };
        // The manifest is its own object (the doc id's preimage), per
        // the assembly convention in OBJECT-STORE-HTTP.md §3.
        index.objects.insert(
            file.document_id,
            file.document.manifest.to_value().encode()?,
        );
        for (id, obj) in file.document.store.iter() {
            index.objects.insert(*id, obj.to_vec());
        }
        index.documents.insert(file.document_id, bytes);
    }
    Ok(index)
}

fn collect_vsd_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_vsd_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("vsd") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// A response, abstract over the HTTP library (testable without sockets).
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub cacheable: bool,
    pub body: Vec<u8>,
}

/// Route a request path per OBJECT-STORE-HTTP.md §1.
pub fn route(index: &Index, path: &str) -> Reply {
    let not_found = || Reply {
        status: 404,
        content_type: "text/plain",
        cacheable: false,
        body: b"not found".to_vec(),
    };
    let parse_id = |hex_part: &str| -> Option<ObjectId> {
        // 64 lowercase hex chars, nothing else (no traversal surface).
        if hex_part.len() != 64 || !hex_part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        hex_part.parse().ok()
    };

    if let Some(hex_part) = path.strip_prefix("/vsd/o/") {
        let Some(id) = parse_id(hex_part) else {
            return not_found();
        };
        match index.objects.get(&id) {
            Some(bytes) => Reply {
                status: 200,
                content_type: "application/vsd-object",
                cacheable: true,
                body: bytes.clone(),
            },
            None => not_found(),
        }
    } else if let Some(hex_part) = path.strip_prefix("/vsd/d/") {
        let Some(id) = parse_id(hex_part) else {
            return not_found();
        };
        match index.documents.get(&id) {
            Some(bytes) => Reply {
                status: 200,
                content_type: "application/vsd",
                cacheable: true,
                body: bytes.clone(),
            },
            None => not_found(),
        }
    } else {
        not_found()
    }
}

pub fn run(root: &Path, addr: &str) -> Result<()> {
    let index = build_index(root)?;
    println!(
        "indexed {} object(s) across {} document(s) from {}",
        index.objects.len(),
        index.documents.len(),
        root.display()
    );
    let server =
        tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    println!("serving on http://{addr}  (GET /vsd/o/<id>, /vsd/d/<doc-id>)");
    println!("clients verify every object by hash; this server is untrusted by design");

    for request in server.incoming_requests() {
        let reply = route(&index, request.url());
        let mut response =
            tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
        response.add_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], reply.content_type.as_bytes())
                .expect("static header"),
        );
        if reply.cacheable {
            response.add_header(
                tiny_http::Header::from_bytes(
                    &b"Cache-Control"[..],
                    &b"public, max-age=31536000, immutable"[..],
                )
                .expect("static header"),
            );
        }
        let _ = request.respond(response);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_container::WriteOptions;
    use vsd_core::compose::Compose;

    fn corpus(dir: &Path) -> (ObjectId, ObjectId) {
        std::fs::create_dir_all(dir).unwrap();
        let doc = Compose::new("en")
            .h1("Served")
            .para("over http, verified client-side")
            .finish()
            .unwrap();
        let bytes =
            vsd_container::write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();
        std::fs::write(dir.join("a.vsd"), &bytes).unwrap();
        (doc.document_id().unwrap(), doc.manifest.root)
    }

    #[test]
    fn routes_serve_verified_content_addressed_bytes() {
        let dir = std::env::temp_dir().join("vsd-serve-test");
        let (doc_id, root_id) = corpus(&dir);
        let index = build_index(&dir).unwrap();

        // The object endpoint returns bytes that hash to the id — the
        // client-side verification the protocol relies on.
        let reply = route(&index, &format!("/vsd/o/{}", root_id.to_hex()));
        assert_eq!(reply.status, 200);
        assert!(reply.cacheable);
        assert_eq!(ObjectId::of_bytes(&reply.body), root_id);

        // The manifest is published under the document id (assembly §3).
        let reply = route(&index, &format!("/vsd/o/{}", doc_id.to_hex()));
        assert_eq!(reply.status, 200);
        assert_eq!(ObjectId::of_bytes(&reply.body), doc_id);

        // Whole-container endpoint round-trips through the strict reader.
        let reply = route(&index, &format!("/vsd/d/{}", doc_id.to_hex()));
        assert_eq!(reply.status, 200);
        let file = vsd_container::read_document(&reply.body, &Default::default()).unwrap();
        assert_eq!(file.document_id, doc_id);

        // Unknown ids and malformed paths 404 (no traversal surface).
        assert_eq!(
            route(&index, &format!("/vsd/o/{}", "0".repeat(64))).status,
            404
        );
        assert_eq!(route(&index, "/vsd/o/../../etc/passwd").status, 404);
        assert_eq!(route(&index, "/vsd/o/zz").status, 404);
        assert_eq!(route(&index, "/").status, 404);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn http_smoke_over_a_real_socket() {
        use std::io::{Read, Write};

        let dir = std::env::temp_dir().join("vsd-serve-socket-test");
        let (doc_id, _) = corpus(&dir);
        let index = build_index(&dir).unwrap();

        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let handle = std::thread::spawn(move || {
            // Serve exactly one request, then stop.
            if let Ok(request) = server.recv() {
                let reply = route(&index, request.url());
                let mut response =
                    tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
                response.add_header(
                    tiny_http::Header::from_bytes(
                        &b"Cache-Control"[..],
                        &b"public, max-age=31536000, immutable"[..],
                    )
                    .unwrap(),
                );
                let _ = request.respond(response);
            }
        });

        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "GET /vsd/o/{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            doc_id.to_hex()
        )
        .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        handle.join().unwrap();

        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("immutable"), "{text}");
        // Body after the blank line hashes to the requested id.
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert_eq!(ObjectId::of_bytes(&raw[split..]), doc_id);

        std::fs::remove_dir_all(&dir).ok();
    }
}
