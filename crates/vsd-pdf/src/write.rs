//! Minimal deterministic PDF object writer: classic cross-reference
//! table, sequential object bodies, no dates, no randomness — the same
//! document exports to byte-identical PDF every time.

/// A PDF indirect object id (generation is always 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjId(pub u32);

impl ObjId {
    pub fn r(self) -> String {
        format!("{} 0 R", self.0)
    }
}

pub struct PdfWriter {
    /// 1-based object bodies (the bytes between `n 0 obj` and `endobj`).
    bodies: Vec<Option<Vec<u8>>>,
}

impl PdfWriter {
    pub fn new() -> Self {
        PdfWriter { bodies: Vec::new() }
    }

    pub fn alloc(&mut self) -> ObjId {
        self.bodies.push(None);
        ObjId(self.bodies.len() as u32)
    }

    pub fn set(&mut self, id: ObjId, body: impl Into<Vec<u8>>) {
        self.bodies[(id.0 - 1) as usize] = Some(body.into());
    }

    pub fn add(&mut self, body: impl Into<Vec<u8>>) -> ObjId {
        let id = self.alloc();
        self.set(id, body);
        id
    }

    /// A stream object with the given extra dictionary entries
    /// (`/Length` is added automatically).
    pub fn stream(&mut self, dict_entries: &str, data: &[u8]) -> ObjId {
        let id = self.alloc();
        self.set_stream(id, dict_entries, data);
        id
    }

    pub fn set_stream(&mut self, id: ObjId, dict_entries: &str, data: &[u8]) {
        let mut body = Vec::with_capacity(data.len() + 64);
        body.extend_from_slice(
            format!("<< /Length {} {} >>\nstream\n", data.len(), dict_entries).as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.set(id, body);
    }

    /// Serialize: header, bodies in order, xref, trailer. Writes a
    /// trailer `/ID` (required by PDF/A) when `id` is given; the same 16
    /// bytes fill both array entries (a freshly created file: permanent
    /// id == changing id).
    pub fn finish_with(self, root: ObjId, info: Option<ObjId>, id: Option<[u8; 16]>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n%\xc2\xb5\xc2\xb6\n");

        let mut offsets = Vec::with_capacity(self.bodies.len());
        for (i, body) in self.bodies.iter().enumerate() {
            offsets.push(out.len());
            let body = body.as_deref().unwrap_or(b"null" as &[u8]); // allocated but never set: null object
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }

        let xref_pos = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", self.bodies.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        let mut trailer = format!(
            "trailer\n<< /Size {} /Root {}",
            self.bodies.len() + 1,
            root.r()
        );
        if let Some(info) = info {
            trailer.push_str(&format!(" /Info {}", info.r()));
        }
        if let Some(id) = id {
            let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
            trailer.push_str(&format!(" /ID [<{hex}> <{hex}>]"));
        }
        trailer.push_str(&format!(" >>\nstartxref\n{xref_pos}\n%%EOF\n"));
        out.extend_from_slice(trailer.as_bytes());
        out
    }
}

impl Default for PdfWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Escape a string for a PDF literal string `(…)`.
pub fn pdf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('(');
    for b in s.chars() {
        match b {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                // Non-ASCII: emit as UTF-16BE only when needed — for
                // simplicity, octal-escape each UTF-8 byte (viewers
                // treat these as PDFDocEncoding; acceptable for
                // metadata).
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("\\{byte:03o}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(')');
    out
}

/// zlib-compress for FlateDecode streams (fixed level → deterministic).
pub fn flate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_file_shape() {
        let mut w = PdfWriter::new();
        let pages = w.alloc();
        let page = w.add(format!(
            "<< /Type /Page /Parent {} /MediaBox [0 0 100 100] >>",
            pages.r()
        ));
        w.set(
            pages,
            format!("<< /Type /Pages /Kids [{}] /Count 1 >>", page.r()),
        );
        let root = w.add(format!("<< /Type /Catalog /Pages {} >>", pages.r()));
        let bytes = w.finish_with(root, None, None);
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("xref"));
    }

    #[test]
    fn string_escaping() {
        assert_eq!(pdf_string("a(b)c\\"), "(a\\(b\\)c\\\\)");
    }
}
