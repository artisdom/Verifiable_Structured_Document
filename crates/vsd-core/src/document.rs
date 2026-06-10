//! The in-memory document aggregate: a manifest plus the object store
//! that backs it. Container-agnostic — `vsd-container` handles bytes on
//! disk, this type handles meaning.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::cbor::Value;
use crate::error::{Error, Result};
use crate::manifest::{
    Manifest, Metadata, Profile, Provenance, RenderCache, ResourceTable, VSD_VERSION,
};
use crate::object::{ObjectId, ObjectStore};
use crate::tree::Node;

#[derive(Clone)]
pub struct Document {
    pub manifest: Manifest,
    pub store: ObjectStore,
}

impl Document {
    /// The document identity: BLAKE3 of the canonical manifest encoding
    /// (spec §2.4). This is what signatures sign.
    pub fn document_id(&self) -> Result<ObjectId> {
        self.manifest.document_id()
    }

    pub fn root_node(&self) -> Result<Node> {
        Node::from_value(&self.store.get_value(&self.manifest.root)?)
    }

    pub fn metadata(&self) -> Result<Metadata> {
        Metadata::from_value(&self.store.get_value(&self.manifest.metadata)?)
    }

    pub fn resources(&self) -> Result<ResourceTable> {
        ResourceTable::from_value(&self.store.get_value(&self.manifest.resources)?)
    }

    pub fn render_cache(&self) -> Result<Option<RenderCache>> {
        match self.manifest.render_cache {
            None => Ok(None),
            Some(id) => Ok(Some(RenderCache::from_value(&self.store.get_value(&id)?)?)),
        }
    }

    pub fn provenance(&self) -> Result<Option<Provenance>> {
        match self.manifest.provenance {
            None => Ok(None),
            Some(id) => Ok(Some(Provenance::from_value(&self.store.get_value(&id)?)?)),
        }
    }

    /// Compute the set of object ids reachable from the manifest — the
    /// Merkle closure that *is* the document. Anything in the store but
    /// outside this set is an orphan (and a redaction-leak risk).
    pub fn closure(&self) -> Result<BTreeSet<ObjectId>> {
        let mut seen = BTreeSet::new();

        // Content tree (follows subtree refs, figure resources, math fallbacks).
        self.collect_node_refs(&self.manifest.root, &mut seen)?;

        // Resource table and its blobs.
        seen.insert(self.manifest.resources);
        for (_, entry) in &self.resources()?.entries {
            seen.insert(entry.data);
        }

        seen.insert(self.manifest.metadata);

        if let Some(rc_id) = self.manifest.render_cache {
            seen.insert(rc_id);
            for page in self.render_cache()?.expect("checked").pages {
                seen.insert(page);
            }
        }
        if let Some(p) = self.manifest.provenance {
            seen.insert(p);
        }
        if let Some(pi) = self.manifest.page_index {
            seen.insert(pi);
        }
        if let Some(fl) = self.manifest.field_layer {
            seen.insert(fl);
        }
        // The predecessor manifest is a *reference to history*, not a
        // contained object; it is not part of this document's closure.
        Ok(seen)
    }

    fn collect_node_refs(&self, id: &ObjectId, seen: &mut BTreeSet<ObjectId>) -> Result<()> {
        if !seen.insert(*id) {
            return Ok(());
        }
        let node = Node::from_value(&self.store.get_value(id)?)?;
        self.collect_refs_in_node(&node, seen)
    }

    fn collect_refs_in_node(&self, node: &Node, seen: &mut BTreeSet<ObjectId>) -> Result<()> {
        use crate::tree::Inline;

        fn walk_inlines(inlines: &[Inline], seen: &mut BTreeSet<ObjectId>) {
            for i in inlines {
                match i {
                    Inline::Math(m) => {
                        if let Some(f) = m.fallback {
                            seen.insert(f);
                        }
                    }
                    Inline::Span(s) => walk_inlines(&s.children, seen),
                    Inline::Link(l) => walk_inlines(&l.children, seen),
                    _ => {}
                }
            }
        }

        match node {
            Node::Doc(d) => {
                for c in &d.children {
                    self.collect_refs_in_node(c, seen)?;
                }
            }
            Node::Section(s) => {
                for c in &s.children {
                    self.collect_refs_in_node(c, seen)?;
                }
            }
            Node::Heading(h) => walk_inlines(&h.children, seen),
            Node::Para(p) => walk_inlines(&p.children, seen),
            Node::Table(t) => {
                for row in t.head.iter().chain(&t.body).chain(&t.foot) {
                    for cell in &row.cells {
                        for c in &cell.children {
                            self.collect_refs_in_node(c, seen)?;
                        }
                    }
                }
            }
            Node::Figure(f) => {
                seen.insert(f.res);
                walk_inlines(&f.caption, seen);
            }
            Node::List(l) => {
                for item in &l.items {
                    for c in item {
                        self.collect_refs_in_node(c, seen)?;
                    }
                }
            }
            Node::Math(m) => {
                if let Some(f) = m.fallback {
                    seen.insert(f);
                }
            }
            Node::SubtreeRef(id) => self.collect_node_refs(id, seen)?,
            Node::Code(_) | Node::Field(_) | Node::PageBreakHint | Node::Redacted(_) => {}
        }
        Ok(())
    }

    /// Collect all field nodes in document order (for form validation).
    pub fn fields(&self) -> Result<Vec<crate::tree::Field>> {
        let mut out = Vec::new();
        self.walk_fields(&self.root_node()?, &mut out)?;
        Ok(out)
    }

    fn walk_fields(&self, node: &Node, out: &mut Vec<crate::tree::Field>) -> Result<()> {
        match node {
            Node::Field(f) => out.push(f.clone()),
            Node::Doc(d) => {
                for c in &d.children {
                    self.walk_fields(c, out)?;
                }
            }
            Node::Section(s) => {
                for c in &s.children {
                    self.walk_fields(c, out)?;
                }
            }
            Node::Table(t) => {
                for row in t.head.iter().chain(&t.body).chain(&t.foot) {
                    for cell in &row.cells {
                        for c in &cell.children {
                            self.walk_fields(c, out)?;
                        }
                    }
                }
            }
            Node::List(l) => {
                for item in &l.items {
                    for c in item {
                        self.walk_fields(c, out)?;
                    }
                }
            }
            Node::SubtreeRef(id) => {
                let sub = Node::from_value(&self.store.get_value(id)?)?;
                self.walk_fields(&sub, out)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Builds a fresh document from authored parts, inserting every object
/// into a new store and producing the manifest.
pub struct DocumentBuilder {
    root: Node,
    metadata: Metadata,
    resources: ResourceTable,
    profile: Profile,
    provenance: Option<Provenance>,
    predecessor: Option<ObjectId>,
    /// Pre-existing objects to carry into the store (e.g. resource blobs,
    /// subtree objects referenced from `root`).
    extra_objects: Vec<Value>,
}

impl DocumentBuilder {
    pub fn new(root: Node) -> Self {
        DocumentBuilder {
            root,
            metadata: Metadata::default(),
            resources: ResourceTable::default(),
            profile: Profile::Core,
            provenance: None,
            predecessor: None,
            extra_objects: Vec::new(),
        }
    }

    pub fn metadata(mut self, m: Metadata) -> Self {
        self.metadata = m;
        self
    }

    pub fn resources(mut self, r: ResourceTable) -> Self {
        self.resources = r;
        self
    }

    pub fn profile(mut self, p: Profile) -> Self {
        self.profile = p;
        self
    }

    pub fn provenance(mut self, p: Provenance) -> Self {
        self.provenance = Some(p);
        self
    }

    pub fn predecessor(mut self, id: ObjectId) -> Self {
        self.predecessor = Some(id);
        self
    }

    /// Add an object (e.g. a blob backing a figure) to the store.
    /// Returns the id it will have, so the tree can reference it.
    pub fn add_object(&mut self, v: Value) -> Result<ObjectId> {
        let id = ObjectId::of_value(&v)?;
        self.extra_objects.push(v);
        Ok(id)
    }

    pub fn build(self) -> Result<Document> {
        if !matches!(self.root, Node::Doc(_)) {
            return Err(Error::Schema("content tree root must be a doc node".into()));
        }
        let mut store = ObjectStore::new();
        for v in &self.extra_objects {
            store.put_value(v)?;
        }
        let root = store.put_value(&self.root.to_value()?)?;
        let resources = store.put_value(&self.resources.to_value())?;
        let metadata = store.put_value(&self.metadata.to_value())?;
        let provenance = self
            .provenance
            .as_ref()
            .map(|p| store.put_value(&p.to_value()))
            .transpose()?;
        let manifest = Manifest {
            version: VSD_VERSION,
            root,
            render_cache: None,
            resources,
            metadata,
            provenance,
            page_index: None,
            field_layer: None,
            profile: self.profile,
            predecessor: self.predecessor,
        };
        Ok(Document { manifest, store })
    }
}
