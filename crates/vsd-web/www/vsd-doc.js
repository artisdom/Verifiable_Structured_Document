/**
 * <vsd-doc> — a dependency-free web component that views VSD documents.
 *
 *   <script type="module" src="vsd-doc.js"></script>
 *   <vsd-doc src="agreement.vsd"></vsd-doc>
 *
 * All parsing, validation, layout recomputation, and rasterization run
 * inside vsd_web.wasm (pure Rust). The badge is the point: it reports
 * whether the document is valid and whether its pixels provably match
 * its content tree — a claim a PDF viewer cannot make.
 */

const WASM_URL = new URL("vsd_web.wasm", import.meta.url);
let wasmPromise = null;

function loadWasm() {
  wasmPromise ??= WebAssembly.instantiateStreaming(fetch(WASM_URL), {}).then(
    (r) => r.instance.exports,
  );
  return wasmPromise;
}

function writeBytes(api, bytes) {
  const ptr = api.vsd_alloc(bytes.length);
  new Uint8Array(api.memory.buffer).set(bytes, ptr);
  return ptr;
}

function readResult(api, len) {
  const ptr = api.vsd_buf_ptr();
  return new Uint8Array(api.memory.buffer.slice(ptr, ptr + len));
}

/**
 * Build a transparent, selectable text layer for one page from the
 * `vsd_page_text` JSON ({w_mm, h_mm, runs:[{x,y,size,rtl,text}]}).
 *
 * Everything is positioned in container-relative units so the layer
 * scales with the page image responsively: `%` for left/top, `cqw`
 * (1% of the container's width) for font-size. `y` is the baseline, so
 * the span's top is lifted by ~0.8em (the approximate ascent). The text
 * is the run's logical text, so selection and copy yield source order.
 */
function buildTextLayer(pg) {
  const layer = document.createElement("div");
  layer.className = "textlayer";
  const w = pg.w_mm || 1;
  const h = pg.h_mm || 1;
  for (const r of pg.runs) {
    const span = document.createElement("span");
    span.textContent = r.text;
    const fhMm = (r.size * 25.4) / 72; // font height in mm
    span.style.left = `${(r.x / w) * 100}%`;
    span.style.top = `${((r.y - fhMm * 0.8) / h) * 100}%`;
    span.style.fontSize = `${(fhMm / w) * 100}cqw`;
    if (r.rtl) span.dir = "rtl";
    layer.appendChild(span);
  }
  return layer;
}

const BADGES = {
  match: ["#1e5631", "#d9f2e0", "verified — pixels match content (recomputed)"],
  "fresh-layout": ["#1e5631", "#d9f2e0", "verified — laid out from content"],
  MISMATCH: ["#fff", "#b3261e", "⚠ RENDER CACHE LIES ABOUT CONTENT"],
  "unknown-engine": ["#8a4b16", "#fde9d9", "cache from unknown engine — shown unverified"],
  error: ["#8a4b16", "#fde9d9", "verification error"],
};

class VsdDoc extends HTMLElement {
  static observedAttributes = ["src", "dpi"];

  async connectedCallback() {
    this.attachShadow({ mode: "open" });
    await this.#load();
  }

  attributeChangedCallback() {
    if (this.shadowRoot) this.#load();
  }

  async #load() {
    const root = this.shadowRoot;
    root.innerHTML = `<style>
      :host{display:block;font:14px/1.4 system-ui,sans-serif}
      .badge{padding:.5em .9em;border-radius:6px;margin-bottom:.6em}
      .meta{color:#555;font-size:.85em;margin-bottom:1em;word-break:break-all}
      /* Each page is its own inline-size container so the overlaid text
         layer scales with the page using container-query units (cqw). */
      .page-wrap{position:relative;container-type:inline-size;margin:0 auto 1.2em;
        max-width:100%;box-shadow:0 1px 6px rgba(0,0,0,.25)}
      .page-wrap img{display:block;width:100%;height:auto}
      /* Transparent, positioned spans of the real logical text: the
         browser's native selection and copy run on these, not on pixels. */
      .textlayer{position:absolute;inset:0;overflow:hidden;line-height:1}
      .textlayer span{position:absolute;white-space:pre;color:transparent;
        transform-origin:left top;cursor:text}
      .textlayer span::selection{background:rgba(70,120,255,.35)}
    </style><div class="badge">loading…</div>`;
    const badge = root.querySelector(".badge");

    try {
      const src = this.getAttribute("src");
      if (!src) throw new Error("vsd-doc: missing src attribute");
      const [api, buf] = await Promise.all([
        loadWasm(),
        fetch(src).then((r) => {
          if (!r.ok) throw new Error(`fetch ${src}: ${r.status}`);
          return r.arrayBuffer();
        }),
      ]);

      const bytes = new Uint8Array(buf);
      const ptr = writeBytes(api, bytes);
      const handle = api.vsd_open(ptr, bytes.length);
      api.vsd_free(ptr, bytes.length);
      if (handle === 0) throw new Error("not a valid VSD document (strict reader refused it)");

      try {
        const infoLen = api.vsd_info(handle);
        const info = JSON.parse(new TextDecoder().decode(readResult(api, infoLen)));

        const sigsBad = info.signatures > 0 && info.signatures_valid < info.signatures;
        const state = info.valid && !sigsBad ? info.recompute : sigsBad ? "MISMATCH" : "error";
        const [fg, bg, label] = BADGES[state] ?? BADGES.error;
        badge.style.color = fg;
        badge.style.background = bg;
        const sigNote = sigsBad
          ? ` · ⚠ only ${info.signatures_valid}/${info.signatures} signature(s) verify`
          : info.signatures
            ? ` · ${info.signatures_valid} signature(s) verified in-browser`
            : "";
        badge.textContent = `${info.valid && !sigsBad ? "✓" : "✗"} ${label}${sigNote}`;

        const meta = document.createElement("div");
        meta.className = "meta";
        meta.textContent = `${info.title ?? "(untitled)"} · ${info.pages} page(s) · id ${info.doc_id}`;
        root.appendChild(meta);

        const dpi = Number(this.getAttribute("dpi") ?? 96);
        const dec = new TextDecoder();
        for (let i = 0; i < info.pages; i++) {
          const len = api.vsd_render_page(handle, i, dpi);
          if (len < 0) continue;
          const wrap = document.createElement("div");
          wrap.className = "page-wrap";

          const img = document.createElement("img");
          img.alt = `Page ${i + 1}`;
          // Read the PNG before the next WASM call overwrites the result buffer.
          img.src = URL.createObjectURL(new Blob([readResult(api, len)], { type: "image/png" }));
          wrap.appendChild(img);

          // Overlay a selectable text layer from the page's text runs.
          const tlen = api.vsd_page_text(handle, i);
          if (tlen >= 0) {
            const pg = JSON.parse(dec.decode(readResult(api, tlen)));
            wrap.appendChild(buildTextLayer(pg));
          }
          root.appendChild(wrap);
        }
      } finally {
        api.vsd_close(handle);
      }
    } catch (err) {
      badge.style.background = "#fde9d9";
      badge.style.color = "#8a4b16";
      badge.textContent = `✗ ${err.message}`;
    }
  }
}

customElements.define("vsd-doc", VsdDoc);
