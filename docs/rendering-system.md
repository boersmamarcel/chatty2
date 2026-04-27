# Rendering System

How Chatty transforms raw LLM text into GPU-rendered UI elements.

## Architecture Overview

The rendering pipeline is a multi-stage transformation:

```
Raw LLM text
  → parse_content_segments()     extract <think> blocks
  → parse_markdown_segments()    extract fenced code blocks
  → parse_math_segments()        extract LaTeX expressions
  → CachedParseResult            structured, cacheable IR
  → render_from_cached()         GPUI element tree
```

Every message passes through this pipeline exactly once (or incrementally during
streaming). The result is cached as a `CachedParseResult` so that subsequent
GPUI render frames read from cache instead of re-parsing.

**Key design principle:** Parsing and rendering are fully separated. All parsing
lives in `message_parsing.rs` (pure functions, independently testable). All GPUI
element construction lives in `message_component.rs` and `message_math_render.rs`.

## Module Organization

The rendering system is split across three primary modules plus supporting
component modules:

### `message_parsing.rs` — Parse & Cache Building

**Responsibility:** Transform raw text into `CachedParseResult`. No GPUI element
construction.

| Function | Purpose |
|:---|:---|
| `parse_content_segments(content)` | Extract `<think>`/`<thinking>`/`<thought>` blocks → `Vec<ContentSegment>` |
| `parse_markdown_segments(content, streaming)` | Extract fenced code blocks → `Vec<MarkdownSegment>` |
| `build_cached_parse_result(content, cx)` | Full pipeline for finalized messages → `CachedParseResult` |
| `build_streaming_parse_result(content, prev, cx)` | Incremental pipeline for streaming → `StreamingParseState` |

### `message_math_render.rs` — Math-Aware Element Construction

**Responsibility:** Convert pre-parsed `MathSegment` slices into GPUI elements.
Handles the interleaving of text, inline math SVGs, and block math SVGs.

| Function | Purpose |
|:---|:---|
| `render_math_segments(segments, base_index, cx)` | Main entry: `&[MathSegment]` → `Vec<AnyElement>` |
| `make_math_component(content, is_inline, id, cx)` | Create `MathComponent` with SVG cache lookup |
| `render_inline_math_batch(batch, ...)` | Layout inline math + text as flex rows |

### `message_component.rs` — Top-Level Message Rendering

**Responsibility:** Orchestrate the full render of a `DisplayMessage` including
role styling, attachments, tool call interleaving, thinking blocks, action
buttons, and cache management.

| Function | Purpose |
|:---|:---|
| `render_message(msg, index, ..., cx)` | Public entry point called by `ChatView` |
| `render_from_cached(cached, index, cx)` | Dispatch `CachedParseResult` → GPUI elements |
| `render_cached_markdown_segments(segments, ...)` | Route each `CachedMarkdownSegment` to its component |
| `render_text_segment_cached(text, ..., caches, cx)` | Cache-aware entry: finalized vs streaming path |
| `render_interleaved_content(msg, ...)` | Text interleaved with tool call blocks |

### Supporting Component Modules

| Module | Component | Renders |
|:---|:---|:---|
| `code_block_component.rs` | `CodeBlockComponent` | Syntax-highlighted code with copy button |
| `math_renderer.rs` | `MathComponent` | LaTeX as SVG image (`img()`) with fallback |
| `mermaid_component.rs` | `MermaidComponent` | Mermaid diagrams as SVG with copy-source/copy-PNG buttons |
| `math_parser.rs` | `parse_math_segments()` | Pure parser: text → `Vec<MathSegment>` |
| `syntax_highlighter.rs` | `highlight_code()` | tree-sitter highlighting → `Vec<(Range, HighlightStyle)>` |

## Pipeline Stages

### Stage 1: Content Segment Extraction

```
Raw text → parse_content_segments() → Vec<ContentSegment>
```

Splits the message at `<think>`, `<thinking>`, and `<thought>` tag boundaries.
Unclosed tags (common during streaming) are treated as incomplete thinking blocks.

```rust
enum ContentSegment {
    Text(String),       // Regular content
    Thinking(String),   // Thinking block content
}
```

### Stage 2: Markdown Segment Extraction

```
ContentSegment::Text → parse_markdown_segments(text, streaming) → Vec<MarkdownSegment>
```

Uses a regex (`CODE_BLOCK_REGEX`) to extract fenced code blocks. In streaming
mode, also detects incomplete code blocks (opening `` ``` `` without closing) via
`detect_incomplete_code_block()`.

```rust
enum MarkdownSegment {
    Text(String),
    CodeBlock { language: Option<String>, code: String },
    IncompleteCodeBlock { language: Option<String>, code: String },  // streaming only
}
```

### Stage 3: Math Segment Analysis

```
MarkdownSegment::Text → parse_math_segments(text) → Vec<MathSegment>
```

The `MathParser` (in `math_parser.rs`) scans for LaTeX delimiters:

- **Inline:** `$...$`, `\(...\)`
- **Block:** `$$...$$` (with blank lines), `\[...\]`, `` ```math ``, `` ```latex ``
- **Environments:** `\begin{equation}`, `\begin{align}`, `\begin{cases}`, etc.

```rust
enum MathSegment {
    Text(String),
    InlineMath(String),
    BlockMath(String),
}
```

The parser handles escaped dollars (`\$`), nested environments with depth
tracking, and heuristic block-vs-inline detection for `$$` based on surrounding
blank lines.

### Stage 4: Cache Building

```
Vec<ContentSegment> → build_cached_parse_result() → CachedParseResult
```

This stage applies expensive transformations:

1. **Syntax highlighting** — `highlight_code(code, language, cx)` via tree-sitter
2. **Math parsing** — `parse_math_segments(text)` for text segments
3. **Mermaid rendering** — `MermaidRendererService::render_to_svg_file()` for
   `language == "mermaid"` code blocks

```rust
struct CachedParseResult {
    segments: Vec<CachedContentSegment>,
}

enum CachedContentSegment {
    Text(Vec<CachedMarkdownSegment>),
    Thinking(String),
}

enum CachedMarkdownSegment {
    TextWithMath(Vec<MathSegment>),
    CodeBlock(CachedCodeBlock),           // language + code + pre-computed styles
    IncompleteCodeBlock { language, code },
    MermaidDiagram { source, svg_path: Option<PathBuf> },
}
```

### Stage 5: Element Construction

```
CachedParseResult → render_from_cached() → Vec<AnyElement>
```

Each `CachedMarkdownSegment` variant dispatches to its component:

| Variant | Component | Notes |
|:---|:---|:---|
| `CodeBlock` | `CodeBlockComponent::with_highlighted_styles()` | Styles pre-computed |
| `TextWithMath` | `render_math_segments()` | Routes to `MathComponent` or `MarkdownContent` |
| `IncompleteCodeBlock` | `CodeBlockComponent::with_highlighted_styles(vec![])` | No highlighting |
| `MermaidDiagram` | `MermaidComponent::with_svg_path()` or `::new()` | SVG or fallback |

## Caching Strategy

### Cache Key

```rust
struct ContentCacheKey(u64);  // hash(content + is_dark_theme)
```

The cache key incorporates theme mode because syntax highlighting styles are
theme-dependent. Two entries exist for the same message if the user switches
between light and dark themes.

### Cache Lifecycle

The `ParsedContentCache` is owned by `ChatView` and passed into rendering
functions via `MessageRenderCaches`:

```rust
struct MessageRenderCaches<'a> {
    parsed: &'a mut ParsedContentCache,       // finalized messages
    streaming: &'a mut Option<StreamingParseState>,  // current stream
}
```

**Finalized messages:** Looked up in `ParsedContentCache` by `ContentCacheKey`.
On miss, `build_cached_parse_result()` runs once and the result is inserted.
Subsequent renders are free lookups.

**Streaming messages:** Use `StreamingParseState` (see next section). On stream
finalization, the streaming cache is cleared and the finalized content enters the
persistent cache on next render.

### Eviction

`ParsedContentCache` is bounded to 200 entries (LRU by insertion order). It is
fully cleared on conversation switch (`ChatView::load_messages()`,
`ChatView::clear_messages()`).

### Cache Invalidation Events

| Event | Action |
|:---|:---|
| Conversation switch | `parsed_cache.clear()`, `streaming_parse_cache = None` |
| Stream finalization | `streaming_parse_cache = None` |
| Theme change | New `ContentCacheKey` (different `is_dark`), old entries eventually evicted |

## Streaming Incremental Reuse

During streaming, content only grows at the end. `build_streaming_parse_result()`
exploits this property with a two-level reuse strategy:

### Level 1: Content Segment Reuse

```
Previous: [Thinking("..."), Text("stable text"), Text("growing...")]
Current:  [Thinking("..."), Text("stable text"), Text("growing... more")]
                                                       ↑ only this re-parsed
```

If the content segment count is unchanged and the content length only grew,
all segments except the last are cloned from the previous `StreamingParseState`.

### Level 2: Markdown Segment Reuse

Within the last text segment, if the markdown segment count is unchanged:

```
Previous md: [Text("intro"), CodeBlock{rust, ...}, Text("tail...")]
Current md:  [Text("intro"), CodeBlock{rust, ...}, Text("tail... more")]
                                                         ↑ only this re-parsed
```

All markdown segments except the last are reused. Completed code blocks are
matched by `try_reuse_code_block()` (language + code equality) to reuse their
expensive tree-sitter highlight styles.

### Tracking State

```rust
struct StreamingParseState {
    result: CachedParseResult,
    content_len: usize,                 // for growth detection
    content_segment_count: usize,       // for segment-level reuse
    last_text_md_count: usize,          // for markdown-level reuse
}
```

### Transition Cost

When segment counts change (e.g., a code block fence closes, a think tag
completes), the affected segment is fully re-parsed — a one-time cost. The
system falls back to `parse_content_segment_streaming_fresh()` for that frame
and resumes incremental reuse on the next frame.

## Math Rendering

### Pipeline: LaTeX → Typst → SVG → GPUI

```
LaTeX expression
  → MiTeX (LaTeX→Typst conversion)
  → Typst (Typst→SVG compilation)
  → strip_svg_dimensions()          remove fixed pt sizes
  → render_to_svg_file()            write {hash}.svg to disk
  → inject_svg_color()              theme color injection
  → render_to_styled_svg_file()     write {hash}.styled.{color_hash}.svg
  → MathComponent                   GPUI img() element
```

### Disk Cache Structure

Located in platform-specific app data directories (e.g.,
`~/Library/Application Support/chatty/math_cache/` on macOS):

| File Pattern | Contents | Lifetime |
|:---|:---|:---|
| `{hash}.svg` | Base SVG (no colors) | Indefinite |
| `{hash}.styled.{color_hash}.svg` | Theme-colored SVG variant | Cleaned on app restart |

The `{hash}` is derived from the LaTeX content + inline/block flag. The
`{color_hash}` is SHA-256 of the hex foreground color.

### In-Memory Cache

`MathRendererService` maintains an `Arc<Mutex<HashMap<String, String>>>` for
the LaTeX → SVG string conversion (before writing to disk). This avoids
re-running Typst for the same expression within a session.

### Theme Color Injection

`inject_svg_color()` replaces `fill="#000000"` and `stroke="#000000"` attributes
in the SVG with the current theme foreground color. This avoids CSS-based
approaches that caused rendering warnings.

### Element Layout

| Math Type | Layout |
|:---|:---|
| **Block math** | Centered `div` with `img()`, max 800×400px, copy-LaTeX button overlay |
| **Inline math** | Flex row with `img()`, max 200×32px, interleaved with text divs |

When SVG is unavailable, `MathComponent` renders a monospace fallback with the
raw LaTeX source.

## Mermaid Diagram Rendering

### Pipeline: Mermaid → SVG → GPUI

```
Mermaid source code (from ```mermaid code block)
  → mermaid_rs_renderer::render_with_options()   native Rust rendering
  → sanitize_svg()                                fix XML issues, cap dimensions
  → render_to_svg_file()                          write {hash}.svg to disk
  → MermaidComponent                              GPUI img() element
```

### Service: `MermaidRendererService`

- Uses `mermaid-rs-renderer` crate (native Rust, no external process)
- Applies dark/light theme via `RenderOptions`
- Sanitizes font-family attributes to avoid XML escaping bugs
- In-memory LRU cache + disk cache (like math)

### Element Layout

Rendered diagrams display as centered `img()` elements (max 800×600px) with
two overlay buttons: copy-as-PNG and copy-source. On Linux, PNG clipboard
writing uses `wl-copy` or `xclip` since GPUI's Linux clipboard silently
discards image data.

Falls back to styled monospace source display when rendering fails.

## Code Block Rendering

### Pipeline: Code → tree-sitter → Styled Text

```
Code string + language
  → SyntaxHighlighter::new(lang)     tree-sitter parser init
  → highlighter.update(None, &rope)  parse into AST
  → highlighter.styles(range, theme) extract highlight spans
  → Vec<(Range<usize>, HighlightStyle)>
  → StyledText::new(code).with_highlights(styles)
  → CodeBlockComponent               GPUI element
```

### `syntax_highlighter::highlight_code()`

Uses `gpui_component::highlighter::SyntaxHighlighter` backed by tree-sitter.
Returns byte-range highlight styles. Regions not covered by any range render
with the theme foreground color. Returns empty `Vec` for unknown languages.

### Caching

Syntax highlighting results are cached within `CachedCodeBlock.styles`. During
streaming, `try_reuse_code_block()` checks if a previous frame has a code block
with matching `language` and `code` content, and reuses its styles directly.

### `CodeBlockComponent`

Renders as a bordered, rounded box with:
- Monospace font at 13px, 1.5× line height
- Pre-computed `StyledText` with highlight spans
- Absolute-positioned copy button (top-right)

## Data Flow Diagram

```
                            ChatView::render_message_list()
                                        │
                                        ▼
                              ┌─────────────────────┐
                              │   render_message()   │
                              │ (message_component)  │
                              └────────┬────────────┘
                                       │
                          ┌────────────┴────────────┐
                          ▼                         ▼
                   is_streaming?              not streaming?
                          │                         │
                          ▼                         ▼
          build_streaming_parse_result()   build_cached_parse_result()
           (incremental, reuses prev)       (full parse, cached by key)
                          │                         │
                          └────────┬────────────────┘
                                   ▼
                          CachedParseResult
                       ┌──────────┴──────────┐
                       ▼                     ▼
              CachedContentSegment    CachedContentSegment
              ::Text(md_segments)     ::Thinking(text)
                       │                     │
                       ▼                     ▼
              render_cached_          render_thinking_block()
              markdown_segments()
                       │
          ┌────────────┼──────────────┬──────────────────┐
          ▼            ▼              ▼                   ▼
    TextWithMath    CodeBlock    IncompleteCodeBlock  MermaidDiagram
          │            │              │                   │
          ▼            ▼              ▼                   ▼
   render_math_   CodeBlock      CodeBlock          MermaidComponent
   segments()     Component      Component          (with_svg_path or
          │       (with styles)  (empty styles)      fallback)
          │
    ┌─────┴──────┐
    ▼            ▼
 BlockMath    InlineMath + Text
    │              │
    ▼              ▼
 MathComponent  flex_row with
 (centered)     MathComponent + text divs

  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─

  External Services (in chatty-core):

  MathRendererService          MermaidRendererService
  ├─ LaTeX → MiTeX → Typst    ├─ mermaid-rs-renderer
  ├─ SVG → inject_svg_color    ├─ sanitize_svg
  ├─ In-memory LRU cache       ├─ In-memory LRU cache
  └─ Disk cache (math_cache/)  └─ Disk cache (mermaid_cache/)
```

## Entry Point: How ChatView Drives Rendering

`ChatView` owns the caches and drives rendering in `render_message_list()`:

```rust
// ChatView fields
parsed_cache: ParsedContentCache,
streaming_parse_cache: Option<StreamingParseState>,
```

On each render frame:
1. Caches are temporarily moved out (`std::mem::take`) to avoid split borrows
2. Each visible `DisplayMessage` calls `render_message()` with a
   `MessageRenderCaches` reference
3. Only the actively streaming message uses the `streaming_parse_cache`; all
   other messages get a throwaway `&mut None`
4. Caches are moved back after all messages are rendered

This ensures the streaming cache is never shared across multiple messages
(which would corrupt its incremental-reuse state).
