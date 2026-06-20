//! Manifest and root-level objects (spec §2.4, §4, §5, §8).

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::cbor::{MapBuilder, Value};
use crate::error::{Error, Result};
use crate::object::ObjectId;

/// Format version written into new manifests. Minor 2 added the
/// `salted` node type; minor 3 added the `rtl` flag on display-list
/// text runs; minor 4 added the `glyphs` display op for pre-shaped
/// complex-script runs (additive within major 0; readers gate on major).
pub const VSD_VERSION: (u16, u16) = (0, 4);

/// Conformance profile (spec §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Core,
    Archive,
    Form,
    Stream,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Core => "core",
            Profile::Archive => "archive",
            Profile::Form => "form",
            Profile::Stream => "stream",
        }
    }

    pub fn parse(s: &str) -> Result<Profile> {
        Ok(match s {
            "core" => Profile::Core,
            "archive" => Profile::Archive,
            "form" => Profile::Form,
            "stream" => Profile::Stream,
            other => return Err(Error::Schema(format!("unknown profile {other:?}"))),
        })
    }
}

/// The root object. **The document's identity is BLAKE3(manifest)** —
/// a single 32-byte value committing, Merkle-style, to every byte of
/// content. This is what signatures sign.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: (u16, u16),
    /// Content tree root object.
    pub root: ObjectId,
    /// Layout projection (spec §5). Optional in Core profile.
    pub render_cache: Option<ObjectId>,
    /// Resource table object.
    pub resources: ObjectId,
    /// Metadata object.
    pub metadata: ObjectId,
    /// C2PA-compatible provenance chain (spec §8).
    pub provenance: Option<ObjectId>,
    /// Streaming page index (spec §9).
    pub page_index: Option<ObjectId>,
    /// Filled form values (spec §6): a separate object layer over the
    /// immutable blank form. The blank form and every filled instance
    /// share all structural objects.
    pub field_layer: Option<ObjectId>,
    pub profile: Profile,
    /// Previous manifest in an authenticated amendment chain (spec §7.1).
    pub predecessor: Option<ObjectId>,
}

impl Manifest {
    pub fn to_value(&self) -> Value {
        MapBuilder::new()
            .put(
                "vsd-version",
                Value::Array(vec![
                    Value::Unsigned(self.version.0 as u64),
                    Value::Unsigned(self.version.1 as u64),
                ]),
            )
            .put("root", self.root.to_value())
            .put_opt("render-cache", self.render_cache.map(ObjectId::to_value))
            .put("resources", self.resources.to_value())
            .put("metadata", self.metadata.to_value())
            .put_opt("provenance", self.provenance.map(ObjectId::to_value))
            .put_opt("page-index", self.page_index.map(ObjectId::to_value))
            .put_opt("field-layer", self.field_layer.map(ObjectId::to_value))
            .put("profile", Value::text(self.profile.as_str()))
            .put_opt("predecessor", self.predecessor.map(ObjectId::to_value))
            .build()
    }

    pub fn from_value(v: &Value) -> Result<Manifest> {
        let allowed = [
            "vsd-version",
            "root",
            "render-cache",
            "resources",
            "metadata",
            "provenance",
            "page-index",
            "field-layer",
            "profile",
            "predecessor",
        ];
        for (k, _) in v
            .as_map()
            .ok_or_else(|| Error::Schema("manifest must be a map".into()))?
        {
            let key = k
                .as_text()
                .ok_or_else(|| Error::Schema("manifest: non-text key".into()))?;
            if !allowed.contains(&key) {
                return Err(Error::Schema(format!("manifest: unknown key {key:?}")));
            }
        }
        let ver = v
            .get("vsd-version")
            .and_then(Value::as_array)
            .filter(|a| a.len() == 2)
            .ok_or_else(|| Error::Schema("manifest: vsd-version must be [major, minor]".into()))?;
        let major = ver[0]
            .as_u64()
            .filter(|&n| n <= u16::MAX as u64)
            .ok_or_else(|| Error::Schema("manifest: bad major version".into()))?
            as u16;
        let minor = ver[1]
            .as_u64()
            .filter(|&n| n <= u16::MAX as u64)
            .ok_or_else(|| Error::Schema("manifest: bad minor version".into()))?
            as u16;

        let opt_ref = |key: &str| -> Result<Option<ObjectId>> {
            v.get(key).map(ObjectId::from_value).transpose()
        };
        let req_ref = |key: &str| -> Result<ObjectId> {
            ObjectId::from_value(
                v.get(key)
                    .ok_or_else(|| Error::Schema(format!("manifest: missing {key:?}")))?,
            )
        };

        Ok(Manifest {
            version: (major, minor),
            root: req_ref("root")?,
            render_cache: opt_ref("render-cache")?,
            resources: req_ref("resources")?,
            metadata: req_ref("metadata")?,
            provenance: opt_ref("provenance")?,
            page_index: opt_ref("page-index")?,
            field_layer: opt_ref("field-layer")?,
            profile: Profile::parse(
                v.get("profile")
                    .and_then(Value::as_text)
                    .ok_or_else(|| Error::Schema("manifest: missing profile".into()))?,
            )?,
            predecessor: opt_ref("predecessor")?,
        })
    }

    /// The document identity: BLAKE3 of the canonical manifest encoding.
    pub fn document_id(&self) -> Result<ObjectId> {
        ObjectId::of_value(&self.to_value())
    }
}

/// Dublin Core-ish metadata plus a custom string map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// RFC 3339 timestamps, author-supplied (the format has no clock).
    pub created: Option<String>,
    pub modified: Option<String>,
    pub keywords: Vec<String>,
    pub custom: Vec<(String, String)>,
}

impl Metadata {
    pub fn to_value(&self) -> Value {
        let custom = Value::Map(
            self.custom
                .iter()
                .map(|(k, v)| (Value::text(k), Value::text(v)))
                .collect(),
        );
        MapBuilder::new()
            .put_opt("title", self.title.as_deref().map(Value::text))
            .put(
                "authors",
                Value::Array(self.authors.iter().map(Value::text).collect()),
            )
            .put_opt("created", self.created.as_deref().map(Value::text))
            .put_opt("modified", self.modified.as_deref().map(Value::text))
            .put(
                "keywords",
                Value::Array(self.keywords.iter().map(Value::text).collect()),
            )
            .put("custom", custom)
            .build()
    }

    pub fn from_value(v: &Value) -> Result<Metadata> {
        let text_array = |key: &str| -> Result<Vec<String>> {
            v.get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| Error::Schema(format!("metadata: {key} must be an array")))?
                .iter()
                .map(|x| {
                    x.as_text()
                        .map(str::to_owned)
                        .ok_or_else(|| Error::Schema(format!("metadata: {key} items must be text")))
                })
                .collect()
        };
        let opt_text = |key: &str| -> Result<Option<String>> {
            match v.get(key) {
                None => Ok(None),
                Some(x) => x
                    .as_text()
                    .map(|s| Some(s.to_owned()))
                    .ok_or_else(|| Error::Schema(format!("metadata: {key} must be text"))),
            }
        };
        let custom = v
            .get("custom")
            .and_then(Value::as_map)
            .ok_or_else(|| Error::Schema("metadata: custom must be a map".into()))?
            .iter()
            .map(|(k, val)| {
                Ok((
                    k.as_text()
                        .ok_or_else(|| Error::Schema("metadata: custom key must be text".into()))?
                        .to_owned(),
                    val.as_text()
                        .ok_or_else(|| Error::Schema("metadata: custom value must be text".into()))?
                        .to_owned(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Metadata {
            title: opt_text("title")?,
            authors: text_array("authors")?,
            created: opt_text("created")?,
            modified: opt_text("modified")?,
            keywords: text_array("keywords")?,
            custom,
        })
    }
}

/// Resource kinds (spec §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    /// JPEG XL / AVIF raster image.
    Image,
    /// VSD-V closed SVG subset.
    Vector,
    /// WOFF2 font subset.
    Font,
    /// ICC color profile.
    IccProfile,
    /// Opaque attachment (e.g. embedded original PDF for migration).
    Attachment,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Image => "image",
            ResourceKind::Vector => "vector",
            ResourceKind::Font => "font",
            ResourceKind::IccProfile => "icc",
            ResourceKind::Attachment => "attachment",
        }
    }

    pub fn parse(s: &str) -> Result<ResourceKind> {
        Ok(match s {
            "image" => ResourceKind::Image,
            "vector" => ResourceKind::Vector,
            "font" => ResourceKind::Font,
            "icc" => ResourceKind::IccProfile,
            "attachment" => ResourceKind::Attachment,
            other => return Err(Error::Schema(format!("unknown resource kind {other:?}"))),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceEntry {
    pub kind: ResourceKind,
    pub mime: String,
    /// Blob object holding the bytes.
    pub data: ObjectId,
}

/// Character style for spans (referenced by index from `Span::style`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub mono: bool,
}

/// The resource table object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceTable {
    /// Named resources, e.g. "logo" → image entry.
    pub entries: Vec<(String, ResourceEntry)>,
    /// Style table for inline spans.
    pub styles: Vec<Style>,
}

impl ResourceTable {
    pub fn to_value(&self) -> Value {
        let entries = Value::Map(
            self.entries
                .iter()
                .map(|(name, e)| {
                    (
                        Value::text(name),
                        MapBuilder::new()
                            .put("kind", Value::text(e.kind.as_str()))
                            .put("mime", Value::text(&e.mime))
                            .put("data", e.data.to_value())
                            .build(),
                    )
                })
                .collect(),
        );
        let styles = Value::Array(
            self.styles
                .iter()
                .map(|s| {
                    MapBuilder::new()
                        .put_opt(
                            "b",
                            if s.bold {
                                Some(Value::Bool(true))
                            } else {
                                None
                            },
                        )
                        .put_opt(
                            "i",
                            if s.italic {
                                Some(Value::Bool(true))
                            } else {
                                None
                            },
                        )
                        .put_opt(
                            "u",
                            if s.underline {
                                Some(Value::Bool(true))
                            } else {
                                None
                            },
                        )
                        .put_opt(
                            "mono",
                            if s.mono {
                                Some(Value::Bool(true))
                            } else {
                                None
                            },
                        )
                        .build()
                })
                .collect(),
        );
        MapBuilder::new()
            .put("entries", entries)
            .put("styles", styles)
            .build()
    }

    pub fn from_value(v: &Value) -> Result<ResourceTable> {
        let entries = v
            .get("entries")
            .and_then(Value::as_map)
            .ok_or_else(|| Error::Schema("resources: entries must be a map".into()))?
            .iter()
            .map(|(k, e)| {
                let name = k
                    .as_text()
                    .ok_or_else(|| Error::Schema("resources: entry name must be text".into()))?;
                let kind = ResourceKind::parse(
                    e.get("kind")
                        .and_then(Value::as_text)
                        .ok_or_else(|| Error::Schema("resource: missing kind".into()))?,
                )?;
                let mime = e
                    .get("mime")
                    .and_then(Value::as_text)
                    .ok_or_else(|| Error::Schema("resource: missing mime".into()))?
                    .to_owned();
                let data = ObjectId::from_value(
                    e.get("data")
                        .ok_or_else(|| Error::Schema("resource: missing data".into()))?,
                )?;
                Ok((name.to_owned(), ResourceEntry { kind, mime, data }))
            })
            .collect::<Result<Vec<_>>>()?;
        let styles = v
            .get("styles")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Schema("resources: styles must be an array".into()))?
            .iter()
            .map(|s| {
                let flag = |key: &str| s.get(key).and_then(Value::as_bool).unwrap_or(false);
                Ok(Style {
                    bold: flag("b"),
                    italic: flag("i"),
                    underline: flag("u"),
                    mono: flag("mono"),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ResourceTable { entries, styles })
    }
}

/// A binary blob object (image bytes, font bytes, …).
#[derive(Clone, Debug, PartialEq)]
pub struct Blob {
    pub mime: String,
    pub data: Vec<u8>,
}

impl Blob {
    pub fn to_value(&self) -> Value {
        MapBuilder::new()
            .put("t", Value::text("blob"))
            .put("mime", Value::text(&self.mime))
            .put("data", Value::Bytes(self.data.clone()))
            .build()
    }

    pub fn from_value(v: &Value) -> Result<Blob> {
        if v.get("t").and_then(Value::as_text) != Some("blob") {
            return Err(Error::Schema("not a blob object".into()));
        }
        Ok(Blob {
            mime: v
                .get("mime")
                .and_then(Value::as_text)
                .ok_or_else(|| Error::Schema("blob: missing mime".into()))?
                .to_owned(),
            data: v
                .get("data")
                .and_then(Value::as_bytes)
                .ok_or_else(|| Error::Schema("blob: missing data".into()))?
                .to_vec(),
        })
    }
}

/// Render cache (spec §5): a deterministic projection of the content tree
/// through a versioned layout engine. `layout_hash` is BLAKE3 of the
/// canonical encoding of the `pages` array, letting any validator detect
/// a cache that lies about its page list; full recomputation requires the
/// named layout engine (see [`crate::layout::LayoutEngine`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RenderCache {
    pub engine_name: String,
    pub engine_version: String,
    /// Page geometry in millimetres.
    pub width_mm: f64,
    pub height_mm: f64,
    /// Display-list objects, one per page.
    pub pages: Vec<ObjectId>,
    pub layout_hash: [u8; 32],
}

impl RenderCache {
    pub fn compute_layout_hash(pages: &[ObjectId]) -> Result<[u8; 32]> {
        let v = Value::Array(pages.iter().copied().map(ObjectId::to_value).collect());
        Ok(*blake3::hash(&v.encode()?).as_bytes())
    }

    pub fn to_value(&self) -> Value {
        MapBuilder::new()
            .put(
                "layout-engine",
                MapBuilder::new()
                    .put("name", Value::text(&self.engine_name))
                    .put("version", Value::text(&self.engine_version))
                    .build(),
            )
            .put(
                "geometry",
                MapBuilder::new()
                    .put("w", Value::Float(self.width_mm))
                    .put("h", Value::Float(self.height_mm))
                    .put("unit", Value::text("mm"))
                    .build(),
            )
            .put(
                "pages",
                Value::Array(self.pages.iter().copied().map(ObjectId::to_value).collect()),
            )
            .put("layout-hash", Value::Bytes(self.layout_hash.to_vec()))
            .build()
    }

    pub fn from_value(v: &Value) -> Result<RenderCache> {
        let engine = v
            .get("layout-engine")
            .ok_or_else(|| Error::Schema("render-cache: missing layout-engine".into()))?;
        let geometry = v
            .get("geometry")
            .ok_or_else(|| Error::Schema("render-cache: missing geometry".into()))?;
        if geometry.get("unit").and_then(Value::as_text) != Some("mm") {
            return Err(Error::Schema(
                "render-cache: geometry unit must be \"mm\"".into(),
            ));
        }
        let pages = v
            .get("pages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Schema("render-cache: pages must be an array".into()))?
            .iter()
            .map(ObjectId::from_value)
            .collect::<Result<Vec<_>>>()?;
        let layout_hash: [u8; 32] = v
            .get("layout-hash")
            .and_then(Value::as_bytes)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| Error::Schema("render-cache: layout-hash must be 32 bytes".into()))?;
        Ok(RenderCache {
            engine_name: engine
                .get("name")
                .and_then(Value::as_text)
                .ok_or_else(|| Error::Schema("render-cache: missing engine name".into()))?
                .to_owned(),
            engine_version: engine
                .get("version")
                .and_then(Value::as_text)
                .ok_or_else(|| Error::Schema("render-cache: missing engine version".into()))?
                .to_owned(),
            width_mm: geometry
                .get("w")
                .and_then(Value::as_f64)
                .ok_or_else(|| Error::Schema("render-cache: missing width".into()))?,
            height_mm: geometry
                .get("h")
                .and_then(Value::as_f64)
                .ok_or_else(|| Error::Schema("render-cache: missing height".into()))?,
            pages,
            layout_hash,
        })
    }
}

/// Streaming page index (spec §9): `page number → required object-id
/// closure`, so a ranged-HTTP client can fetch exactly the objects for
/// page 47 of a 900-page manual. Produced alongside a render cache;
/// VSD/Stream requires it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageIndex {
    /// `pages[n]` lists every object needed to render page `n`.
    pub pages: Vec<Vec<ObjectId>>,
}

impl PageIndex {
    pub fn to_value(&self) -> Value {
        MapBuilder::new()
            .put("t", Value::text("page-index"))
            .put(
                "pages",
                Value::Array(
                    self.pages
                        .iter()
                        .map(|ids| {
                            Value::Array(ids.iter().copied().map(ObjectId::to_value).collect())
                        })
                        .collect(),
                ),
            )
            .build()
    }

    pub fn from_value(v: &Value) -> Result<PageIndex> {
        if v.get("t").and_then(Value::as_text) != Some("page-index") {
            return Err(Error::Schema("not a page-index object".into()));
        }
        let pages = v
            .get("pages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Schema("page-index: pages must be an array".into()))?
            .iter()
            .map(|p| {
                p.as_array()
                    .ok_or_else(|| Error::Schema("page-index: page entry must be an array".into()))?
                    .iter()
                    .map(ObjectId::from_value)
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PageIndex { pages })
    }
}

/// Provenance assertion (spec §8) — C2PA-compatible custody chain entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Assertion {
    /// e.g. "created-by", "derived-from", "ai-generated",
    /// "scanned-from-physical", "format-migrated".
    pub kind: String,
    /// Free-form claims, e.g. {"tool": "vsd-cli 0.1", "lossy": "false"}.
    pub claims: Vec<(String, String)>,
    /// Manifest hash at this point in history.
    pub manifest_hash: ObjectId,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Provenance {
    pub assertions: Vec<Assertion>,
}

impl Provenance {
    pub fn to_value(&self) -> Value {
        Value::Array(
            self.assertions
                .iter()
                .map(|a| {
                    MapBuilder::new()
                        .put("kind", Value::text(&a.kind))
                        .put(
                            "claims",
                            Value::Map(
                                a.claims
                                    .iter()
                                    .map(|(k, v)| (Value::text(k), Value::text(v)))
                                    .collect(),
                            ),
                        )
                        .put("manifest-hash", a.manifest_hash.to_value())
                        .build()
                })
                .collect(),
        )
    }

    pub fn from_value(v: &Value) -> Result<Provenance> {
        let assertions = v
            .as_array()
            .ok_or_else(|| Error::Schema("provenance must be an array".into()))?
            .iter()
            .map(|a| {
                let kind = a
                    .get("kind")
                    .and_then(Value::as_text)
                    .ok_or_else(|| Error::Schema("assertion: missing kind".into()))?
                    .to_owned();
                let claims = a
                    .get("claims")
                    .and_then(Value::as_map)
                    .ok_or_else(|| Error::Schema("assertion: claims must be a map".into()))?
                    .iter()
                    .map(|(k, v)| {
                        Ok((
                            k.as_text()
                                .ok_or_else(|| Error::Schema("assertion: claim key".into()))?
                                .to_owned(),
                            v.as_text()
                                .ok_or_else(|| Error::Schema("assertion: claim value".into()))?
                                .to_owned(),
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let manifest_hash =
                    ObjectId::from_value(a.get("manifest-hash").ok_or_else(|| {
                        Error::Schema("assertion: missing manifest-hash".into())
                    })?)?;
                Ok(Assertion {
                    kind,
                    claims,
                    manifest_hash,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Provenance { assertions })
    }
}
