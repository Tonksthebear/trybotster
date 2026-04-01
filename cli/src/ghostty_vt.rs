//! Safe Rust wrapper around libghostty-vt's C API.
//!
//! Provides terminal creation, VT byte processing, ANSI/VT formatting,
//! mode queries, effect callbacks, and render state for TUI rendering.
//! This replaces `alacritty_terminal` as the terminal emulator engine.

use std::ffi::c_void;
use std::ptr;

// ---------------------------------------------------------------------------
// Raw FFI bindings
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum GhosttyResult {
    Success = 0,
    OutOfMemory = -1,
    InvalidValue = -2,
    OutOfSpace = -3,
    NoValue = -4,
}

// ── Types ──────────────────────────────────────────────────────────────────

/// Borrowed byte string from the C API. Valid only until the next terminal mutation.
#[repr(C)]
#[derive(Clone, Copy)]
struct GhosttyString {
    ptr: *const u8,
    len: usize,
}

/// RGB color value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GhosttyColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Color scheme value returned for CSI ? 996 n queries.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhosttyColorScheme {
    Light = 0,
    Dark = 1,
}

// ── Terminal options / creation ─────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct GhosttyTerminalOptions {
    cols: u16,
    rows: u16,
    max_scrollback: usize,
}

// ── Modes ──────────────────────────────────────────────────────────────────

/// Packed 16-bit terminal mode. Bits 0-14 = mode value, bit 15 = ANSI flag.
type GhosttyMode = u16;

/// Create a packed mode constant.
const fn ghostty_mode(value: u16, ansi: bool) -> GhosttyMode {
    (value & 0x7FFF) | ((ansi as u16) << 15)
}

// DEC private modes (bit 15 = 0)
pub const MODE_DECCKM: GhosttyMode = ghostty_mode(1, false);
pub const MODE_REVERSE_COLORS: GhosttyMode = ghostty_mode(5, false);
pub const MODE_ORIGIN: GhosttyMode = ghostty_mode(6, false);
pub const MODE_WRAPAROUND: GhosttyMode = ghostty_mode(7, false);
pub const MODE_CURSOR_BLINKING: GhosttyMode = ghostty_mode(12, false);
pub const MODE_CURSOR_VISIBLE: GhosttyMode = ghostty_mode(25, false);
pub const MODE_KEYPAD_KEYS: GhosttyMode = ghostty_mode(66, false);
pub const MODE_NORMAL_MOUSE: GhosttyMode = ghostty_mode(1000, false);
pub const MODE_BUTTON_MOUSE: GhosttyMode = ghostty_mode(1002, false);
pub const MODE_ANY_MOUSE: GhosttyMode = ghostty_mode(1003, false);
pub const MODE_FOCUS_EVENT: GhosttyMode = ghostty_mode(1004, false);
pub const MODE_SGR_MOUSE: GhosttyMode = ghostty_mode(1006, false);
pub const MODE_ALT_SCROLL: GhosttyMode = ghostty_mode(1007, false);
pub const MODE_ALT_SCREEN_SAVE: GhosttyMode = ghostty_mode(1049, false);
pub const MODE_BRACKETED_PASTE: GhosttyMode = ghostty_mode(2004, false);

// ANSI modes (bit 15 = 1)
pub const MODE_INSERT: GhosttyMode = ghostty_mode(4, true);
pub const MODE_LINEFEED: GhosttyMode = ghostty_mode(20, true);

// ── Terminal data queries ──────────────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyTerminalData {
    Invalid = 0,
    Cols = 1,
    Rows = 2,
    CursorX = 3,
    CursorY = 4,
    CursorPendingWrap = 5,
    ActiveScreen = 6,
    CursorVisible = 7,
    KittyKeyboardFlags = 8,
    Scrollbar = 9,
    CursorStyle = 10,
    MouseTracking = 11,
    Title = 12,
    Pwd = 13,
    TotalRows = 14,
    ScrollbackRows = 15,
    WidthPx = 16,
    HeightPx = 17,
    ColorForeground = 18,
    ColorBackground = 19,
    ColorCursor = 20,
    ColorPalette = 21,
    ColorForegroundDefault = 22,
    ColorBackgroundDefault = 23,
    ColorCursorDefault = 24,
    ColorPaletteDefault = 25,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum GhosttyTerminalScreen {
    Primary = 0,
    Alternate = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GhosttyTerminalScrollbar {
    total: u64,
    offset: u64,
    len: u64,
}

/// Scrollbar state for the current terminal viewport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollbarState {
    pub total: usize,
    pub offset: usize,
    pub len: usize,
}

impl ScrollbarState {
    /// Lines between the bottom of the viewport and live output.
    pub fn lines_from_bottom(self) -> usize {
        self.total
            .saturating_sub(self.offset.saturating_add(self.len))
    }

    /// Total scrollback rows available above the live viewport.
    pub fn scrollback_rows(self) -> usize {
        self.total.saturating_sub(self.len)
    }
}

// ── Terminal options (set) ─────────────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyTerminalOption {
    Userdata = 0,
    WritePty = 1,
    Bell = 2,
    Enquiry = 3,
    Xtversion = 4,
    TitleChanged = 5,
    Size = 6,
    ColorScheme = 7,
    DeviceAttributes = 8,
    Title = 9,
    Pwd = 10,
    ColorForeground = 11,
    ColorBackground = 12,
    ColorCursor = 13,
    ColorPalette = 14,
    PwdChanged = 15,
    Notification = 16,
    SemanticPrompt = 17,
    ModeChanged = 18,
    KittyKeyboardChanged = 19,
}

// ── Callback function pointer types ────────────────────────────────────────

type GhosttyTerminalWritePtyFn = Option<
    unsafe extern "C" fn(
        terminal: GhosttyTerminalPtr,
        userdata: *mut c_void,
        data: *const u8,
        len: usize,
    ),
>;

type GhosttyTerminalBellFn =
    Option<unsafe extern "C" fn(terminal: GhosttyTerminalPtr, userdata: *mut c_void)>;

type GhosttyTerminalColorSchemeFn = Option<
    unsafe extern "C" fn(
        terminal: GhosttyTerminalPtr,
        userdata: *mut c_void,
        out_scheme: *mut GhosttyColorScheme,
    ) -> bool,
>;

type GhosttyTerminalTitleChangedFn =
    Option<unsafe extern "C" fn(terminal: GhosttyTerminalPtr, userdata: *mut c_void)>;

type GhosttyTerminalPwdChangedFn =
    Option<unsafe extern "C" fn(terminal: GhosttyTerminalPtr, userdata: *mut c_void)>;

type GhosttyTerminalNotificationFn = Option<
    unsafe extern "C" fn(
        terminal: GhosttyTerminalPtr,
        userdata: *mut c_void,
        title: *const u8,
        title_len: usize,
        body: *const u8,
        body_len: usize,
    ),
>;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhosttySemanticPromptAction {
    FreshLine = 0,
    FreshLineNewPrompt = 1,
    NewCommand = 2,
    PromptStart = 3,
    EndPromptStartInput = 4,
    EndPromptStartInputTerminateEol = 5,
    EndInputStartOutput = 6,
    EndCommand = 7,
}

type GhosttyTerminalSemanticPromptFn = Option<
    unsafe extern "C" fn(
        terminal: GhosttyTerminalPtr,
        userdata: *mut c_void,
        action: GhosttySemanticPromptAction,
    ),
>;

type GhosttyTerminalModeChangedFn = Option<
    unsafe extern "C" fn(
        terminal: GhosttyTerminalPtr,
        userdata: *mut c_void,
        mode: u16,
        enabled: bool,
    ),
>;

type GhosttyTerminalKittyKeyboardChangedFn =
    Option<unsafe extern "C" fn(terminal: GhosttyTerminalPtr, userdata: *mut c_void)>;

// ── Scroll viewport ───────────────────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyScrollViewportTag {
    Top = 0,
    Bottom = 1,
    Delta = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
union GhosttyScrollViewportValue {
    delta: isize,
    _padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GhosttyScrollViewport {
    tag: GhosttyScrollViewportTag,
    value: GhosttyScrollViewportValue,
}

// ── Style ──────────────────────────────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum GhosttyStyleColorTag {
    None = 0,
    Palette = 1,
    Rgb = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyStyleColorValue {
    pub palette: u8,
    pub rgb: GhosttyColorRgb,
    _padding: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyleColor {
    pub tag: GhosttyStyleColorTag,
    pub value: GhosttyStyleColorValue,
}

/// Terminal cell style — sized struct, matches C layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyle {
    pub size: usize,
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: i32,
}

impl GhosttyStyle {
    pub fn default_sized() -> Self {
        let mut s = unsafe { std::mem::zeroed::<Self>() };
        s.size = std::mem::size_of::<Self>();
        s
    }
}

// ── Point ──────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GhosttyPointCoordinate {
    pub x: u16,
    pub y: u32,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum GhosttyPointTag {
    Active = 0,
    Viewport = 1,
    Screen = 2,
    History = 3,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyPointValue {
    pub coordinate: GhosttyPointCoordinate,
    _padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyPoint {
    pub tag: GhosttyPointTag,
    pub value: GhosttyPointValue,
}

// ── Grid ref ───────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyGridRef {
    pub size: usize,
    pub node: *mut c_void,
    pub x: u16,
    pub y: u16,
}

impl GhosttyGridRef {
    pub fn new_sized() -> Self {
        let mut r = unsafe { std::mem::zeroed::<Self>() };
        r.size = std::mem::size_of::<Self>();
        r
    }
}

// ── Cell / Row ─────────────────────────────────────────────────────────────

pub type GhosttyCell = u64;
pub type GhosttyRow = u64;

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum GhosttyCellData {
    Invalid = 0,
    Codepoint = 1,
    ContentTag = 2,
    Wide = 3,
    HasText = 4,
    HasStyling = 5,
    StyleId = 6,
    HasHyperlink = 7,
    Protected = 8,
    SemanticContent = 9,
    ColorPalette = 10,
    ColorRgb = 11,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum GhosttyCellWide {
    Narrow = 0,
    Wide = 1,
    SpacerTail = 2,
    SpacerHead = 3,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum GhosttyRowData {
    Invalid = 0,
    Wrap = 1,
    WrapContinuation = 2,
    Grapheme = 3,
    Styled = 4,
    Hyperlink = 5,
    SemanticPrompt = 6,
    KittyVirtualPlaceholder = 7,
    Dirty = 8,
}

// ── Render state ───────────────────────────────────────────────────────────

#[repr(C)]
pub(crate) struct GhosttyRenderStateOpaque {
    _opaque: [u8; 0],
}
type GhosttyRenderStatePtr = *mut GhosttyRenderStateOpaque;

#[repr(C)]
struct GhosttyRowIteratorOpaque {
    _opaque: [u8; 0],
}
type GhosttyRowIteratorPtr = *mut GhosttyRowIteratorOpaque;

#[repr(C)]
struct GhosttyRowCellsOpaque {
    _opaque: [u8; 0],
}
type GhosttyRowCellsPtr = *mut GhosttyRowCellsOpaque;

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum GhosttyRenderStateDirty {
    False = 0,
    Partial = 1,
    Full = 2,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum GhosttyRenderStateCursorVisualStyle {
    Bar = 0,
    Block = 1,
    Underline = 2,
    BlockHollow = 3,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyRenderStateData {
    Invalid = 0,
    Cols = 1,
    Rows = 2,
    Dirty = 3,
    RowIterator = 4,
    ColorBackground = 5,
    ColorForeground = 6,
    ColorCursor = 7,
    ColorCursorHasValue = 8,
    ColorPalette = 9,
    CursorVisualStyle = 10,
    CursorVisible = 11,
    CursorBlinking = 12,
    CursorPasswordInput = 13,
    CursorViewportHasValue = 14,
    CursorViewportX = 15,
    CursorViewportY = 16,
    CursorViewportWideTail = 17,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyRenderStateOption {
    Dirty = 0,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyRenderStateRowData {
    Invalid = 0,
    Dirty = 1,
    Raw = 2,
    Cells = 3,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyRenderStateRowOption {
    Dirty = 0,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum GhosttyRenderStateRowCellsData {
    Invalid = 0,
    Raw = 1,
    Style = 2,
    GraphemesLen = 3,
    GraphemesBuf = 4,
    BgColor = 5,
    FgColor = 6,
}

/// Render-state color information — sized struct.
#[repr(C)]
pub struct GhosttyRenderStateColors {
    pub size: usize,
    pub background: GhosttyColorRgb,
    pub foreground: GhosttyColorRgb,
    pub cursor: GhosttyColorRgb,
    pub cursor_has_value: bool,
    pub palette: [GhosttyColorRgb; 256],
}

// ── Formatter ──────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum GhosttyFormatterFormat {
    Plain = 0,
    Vt = 1,
    Html = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GhosttyFormatterScreenExtra {
    size: usize,
    cursor: bool,
    style: bool,
    hyperlink: bool,
    protection: bool,
    kitty_keyboard: bool,
    charsets: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GhosttyFormatterTerminalExtra {
    size: usize,
    palette: bool,
    modes: bool,
    scrolling_region: bool,
    tabstops: bool,
    pwd: bool,
    keyboard: bool,
    screen: GhosttyFormatterScreenExtra,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GhosttyFormatterTerminalOptions {
    size: usize,
    emit: GhosttyFormatterFormat,
    unwrap: bool,
    trim: bool,
    extra: GhosttyFormatterTerminalExtra,
}

// ── Opaque handles ─────────────────────────────────────────────────────────

/// Opaque terminal handle — exposed for callback function pointer signatures.
#[repr(C)]
pub struct GhosttyTerminalOpaque {
    _opaque: [u8; 0],
}
type GhosttyTerminalPtr = *mut GhosttyTerminalOpaque;

#[repr(C)]
struct GhosttyFormatterOpaque {
    _opaque: [u8; 0],
}
type GhosttyFormatterPtr = *mut GhosttyFormatterOpaque;

// ── Log callback ──────────────────────────────────────────────────────────

/// Callback invoked by ghostty's Zig logging. Routes through Rust's `log` crate.
extern "C" fn ghostty_log_callback(level: u8, ptr: *const u8, len: usize) {
    // SAFETY: Zig passes a valid pointer+length from a stack buffer.
    let msg = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) };
    match level {
        0 => log::error!(target: "ghostty", "{msg}"),
        1 => log::warn!(target: "ghostty", "{msg}"),
        2 => log::info!(target: "ghostty", "{msg}"),
        _ => log::debug!(target: "ghostty", "{msg}"),
    }
}

/// Install the log callback. Call once at startup before creating any terminals.
pub fn init_logging() {
    unsafe {
        ghostty_vt_set_log_callback(ghostty_log_callback);
    }
}

// ── Extern C functions ─────────────────────────────────────────────────────

#[allow(dead_code)]
extern "C" {
    // Logging
    fn ghostty_vt_set_log_callback(cb: extern "C" fn(level: u8, ptr: *const u8, len: usize));

    // Terminal lifecycle
    fn ghostty_terminal_new(
        allocator: *const c_void,
        terminal: *mut GhosttyTerminalPtr,
        options: GhosttyTerminalOptions,
    ) -> GhosttyResult;
    fn ghostty_terminal_free(terminal: GhosttyTerminalPtr);
    fn ghostty_terminal_reset(terminal: GhosttyTerminalPtr);

    // VT processing
    fn ghostty_terminal_vt_write(terminal: GhosttyTerminalPtr, data: *const u8, len: usize);

    // Resize
    fn ghostty_terminal_resize(
        terminal: GhosttyTerminalPtr,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> GhosttyResult;

    // Mode queries
    fn ghostty_terminal_mode_get(
        terminal: GhosttyTerminalPtr,
        mode: GhosttyMode,
        out_value: *mut bool,
    ) -> GhosttyResult;

    // Terminal data
    fn ghostty_terminal_get(
        terminal: GhosttyTerminalPtr,
        data: GhosttyTerminalData,
        out: *mut c_void,
    ) -> GhosttyResult;

    // Terminal options (set callbacks, colors, etc.)
    fn ghostty_terminal_set(
        terminal: GhosttyTerminalPtr,
        option: GhosttyTerminalOption,
        value: *const c_void,
    ) -> GhosttyResult;

    // Viewport scrolling
    fn ghostty_terminal_scroll_viewport(
        terminal: GhosttyTerminalPtr,
        behavior: GhosttyScrollViewport,
    );

    // Grid ref (for non-render-loop cell access)
    fn ghostty_terminal_grid_ref(
        terminal: GhosttyTerminalPtr,
        point: GhosttyPoint,
        out_ref: *mut GhosttyGridRef,
    ) -> GhosttyResult;

    // Grid ref accessors
    fn ghostty_grid_ref_cell(
        grid_ref: *const GhosttyGridRef,
        out_cell: *mut GhosttyCell,
    ) -> GhosttyResult;
    fn ghostty_grid_ref_row(
        grid_ref: *const GhosttyGridRef,
        out_row: *mut GhosttyRow,
    ) -> GhosttyResult;
    fn ghostty_grid_ref_graphemes(
        grid_ref: *const GhosttyGridRef,
        buf: *mut u32,
        buf_len: usize,
        out_len: *mut usize,
    ) -> GhosttyResult;
    fn ghostty_grid_ref_style(
        grid_ref: *const GhosttyGridRef,
        out_style: *mut GhosttyStyle,
    ) -> GhosttyResult;

    // Cell / row data
    fn ghostty_cell_get(
        cell: GhosttyCell,
        data: GhosttyCellData,
        out: *mut c_void,
    ) -> GhosttyResult;
    fn ghostty_row_get(row: GhosttyRow, data: GhosttyRowData, out: *mut c_void) -> GhosttyResult;

    // Render state
    fn ghostty_render_state_new(
        allocator: *const c_void,
        state: *mut GhosttyRenderStatePtr,
    ) -> GhosttyResult;
    fn ghostty_render_state_free(state: GhosttyRenderStatePtr);
    fn ghostty_render_state_update(
        state: GhosttyRenderStatePtr,
        terminal: GhosttyTerminalPtr,
    ) -> GhosttyResult;
    fn ghostty_render_state_get(
        state: GhosttyRenderStatePtr,
        data: GhosttyRenderStateData,
        out: *mut c_void,
    ) -> GhosttyResult;
    fn ghostty_render_state_set(
        state: GhosttyRenderStatePtr,
        option: GhosttyRenderStateOption,
        value: *const c_void,
    ) -> GhosttyResult;
    fn ghostty_render_state_colors_get(
        state: GhosttyRenderStatePtr,
        out_colors: *mut GhosttyRenderStateColors,
    ) -> GhosttyResult;

    // Row iterator
    fn ghostty_render_state_row_iterator_new(
        allocator: *const c_void,
        out_iterator: *mut GhosttyRowIteratorPtr,
    ) -> GhosttyResult;
    fn ghostty_render_state_row_iterator_free(iterator: GhosttyRowIteratorPtr);
    fn ghostty_render_state_row_iterator_next(iterator: GhosttyRowIteratorPtr) -> bool;
    fn ghostty_render_state_row_get(
        iterator: GhosttyRowIteratorPtr,
        data: GhosttyRenderStateRowData,
        out: *mut c_void,
    ) -> GhosttyResult;
    fn ghostty_render_state_row_set(
        iterator: GhosttyRowIteratorPtr,
        option: GhosttyRenderStateRowOption,
        value: *const c_void,
    ) -> GhosttyResult;

    // Row cells
    fn ghostty_render_state_row_cells_new(
        allocator: *const c_void,
        out_cells: *mut GhosttyRowCellsPtr,
    ) -> GhosttyResult;
    fn ghostty_render_state_row_cells_free(cells: GhosttyRowCellsPtr);
    fn ghostty_render_state_row_cells_next(cells: GhosttyRowCellsPtr) -> bool;
    fn ghostty_render_state_row_cells_select(cells: GhosttyRowCellsPtr, x: u16) -> GhosttyResult;
    fn ghostty_render_state_row_cells_get(
        cells: GhosttyRowCellsPtr,
        data: GhosttyRenderStateRowCellsData,
        out: *mut c_void,
    ) -> GhosttyResult;

    // Formatter
    fn ghostty_formatter_terminal_new(
        allocator: *const c_void,
        formatter: *mut GhosttyFormatterPtr,
        terminal: GhosttyTerminalPtr,
        options: GhosttyFormatterTerminalOptions,
    ) -> GhosttyResult;
    fn ghostty_formatter_format_alloc(
        formatter: GhosttyFormatterPtr,
        allocator: *const c_void,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;
    fn ghostty_formatter_free(formatter: GhosttyFormatterPtr);

    // Memory
    fn ghostty_free(allocator: *const c_void, ptr: *mut u8, len: usize);

    // Style
    fn ghostty_style_default(style: *mut GhosttyStyle);
    fn ghostty_style_is_default(style: *const GhosttyStyle) -> bool;

    // ── Opaque terminal snapshot transfer ───────────────────────────────
    fn ghostty_terminal_snapshot_export(
        terminal: GhosttyTerminalPtr,
        allocator: *const c_void,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;

    fn ghostty_terminal_snapshot_import(
        terminal: GhosttyTerminalPtr,
        data: *const u8,
        data_len: usize,
    ) -> GhosttyResult;
}

// ---------------------------------------------------------------------------
// Safe wrappers
// ---------------------------------------------------------------------------

/// A Ghostty terminal emulator instance.
///
/// Wraps the C `GhosttyTerminal` handle with RAII semantics.
pub struct Terminal {
    handle: GhosttyTerminalPtr,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("handle", &(!self.handle.is_null()))
            .finish()
    }
}

// SAFETY: The C API does not use thread-local state. All access is through
// the handle, and we enforce &mut self for mutations.
unsafe impl Send for Terminal {}

unsafe extern "C" fn builtin_color_scheme_trampoline(
    terminal: GhosttyTerminalPtr,
    _userdata: *mut c_void,
    out_scheme: *mut GhosttyColorScheme,
) -> bool {
    if out_scheme.is_null() {
        return false;
    }

    let mut background = GhosttyColorRgb { r: 0, g: 0, b: 0 };
    let background_ptr = &mut background as *mut GhosttyColorRgb as *mut c_void;
    let result = unsafe {
        ghostty_terminal_get(
            terminal,
            GhosttyTerminalData::ColorBackgroundDefault,
            background_ptr,
        )
    };
    let result = if result == GhosttyResult::Success {
        result
    } else {
        unsafe {
            ghostty_terminal_get(
                terminal,
                GhosttyTerminalData::ColorBackground,
                background_ptr,
            )
        }
    };

    if result != GhosttyResult::Success {
        return false;
    }

    let luma = (u32::from(background.r) * 299)
        + (u32::from(background.g) * 587)
        + (u32::from(background.b) * 114);
    let scheme = if luma / 1000 >= 128 {
        GhosttyColorScheme::Light
    } else {
        GhosttyColorScheme::Dark
    };
    unsafe {
        *out_scheme = scheme;
    }
    true
}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self, &'static str> {
        let opts = GhosttyTerminalOptions {
            cols,
            rows,
            max_scrollback,
        };
        let mut handle: GhosttyTerminalPtr = ptr::null_mut();

        let result = unsafe { ghostty_terminal_new(ptr::null(), &mut handle, opts) };

        match result {
            GhosttyResult::Success => Ok(Terminal { handle }),
            GhosttyResult::OutOfMemory => Err("ghostty_terminal_new: out of memory"),
            _ => Err("ghostty_terminal_new: failed"),
        }
    }

    /// Raw handle for passing to render state update.
    pub(crate) fn handle(&self) -> GhosttyTerminalPtr {
        self.handle
    }

    // ── VT processing ──────────────────────────────────────────────────

    /// Feed VT-encoded bytes into the terminal (as if received from a PTY).
    pub fn write(&mut self, data: &[u8]) {
        unsafe {
            ghostty_terminal_vt_write(self.handle, data.as_ptr(), data.len());
        }
    }

    /// Resize the terminal to new dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), &'static str> {
        let result = unsafe { ghostty_terminal_resize(self.handle, cols, rows, 0, 0) };
        match result {
            GhosttyResult::Success => Ok(()),
            _ => Err("ghostty_terminal_resize: failed"),
        }
    }

    /// Perform a full reset (RIS).
    pub fn reset(&mut self) {
        unsafe { ghostty_terminal_reset(self.handle) };
    }

    // ── Mode queries ───────────────────────────────────────────────────

    /// Query a terminal mode by its packed GhosttyMode constant.
    pub fn mode_get(&self, mode: GhosttyMode) -> bool {
        let mut out = false;
        let result = unsafe { ghostty_terminal_mode_get(self.handle, mode, &mut out) };
        result == GhosttyResult::Success && out
    }

    /// Whether the cursor is currently hidden (`DECTCEM` off).
    pub fn cursor_hidden(&self) -> bool {
        !self.mode_get(MODE_CURSOR_VISIBLE)
    }

    /// Whether the Kitty keyboard protocol is active.
    pub fn kitty_enabled(&self) -> bool {
        let mut flags: u8 = 0;
        let result = unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::KittyKeyboardFlags,
                &mut flags as *mut u8 as *mut c_void,
            )
        };
        result == GhosttyResult::Success && flags > 0
    }

    /// Whether focus reporting mode is active (`CSI ? 1004 h`).
    pub fn focus_reporting(&self) -> bool {
        self.mode_get(MODE_FOCUS_EVENT)
    }

    /// Whether application cursor keys mode is active (`DECCKM`).
    pub fn application_cursor(&self) -> bool {
        self.mode_get(MODE_DECCKM)
    }

    /// Whether bracketed paste mode is active.
    pub fn bracketed_paste(&self) -> bool {
        self.mode_get(MODE_BRACKETED_PASTE)
    }

    /// Whether the alternate screen buffer is active.
    pub fn alt_screen_active(&self) -> bool {
        let mut screen = GhosttyTerminalScreen::Primary;
        let result = unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::ActiveScreen,
                &mut screen as *mut GhosttyTerminalScreen as *mut c_void,
            )
        };
        result == GhosttyResult::Success && screen == GhosttyTerminalScreen::Alternate
    }

    /// Mouse tracking mode as a bitmask (0 = off).
    pub fn mouse_mode(&self) -> u8 {
        let mut flags = 0u8;
        if self.mode_get(MODE_NORMAL_MOUSE) {
            flags |= 1;
        }
        if self.mode_get(MODE_ANY_MOUSE) {
            flags |= 2;
        }
        if self.mode_get(MODE_BUTTON_MOUSE) {
            flags |= 4;
        }
        if self.mode_get(MODE_SGR_MOUSE) {
            flags |= 8;
        }
        flags
    }

    /// Whether any mouse tracking mode is active.
    pub fn mouse_tracking(&self) -> bool {
        let mut tracking = false;
        let result = unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::MouseTracking,
                &mut tracking as *mut bool as *mut c_void,
            )
        };
        result == GhosttyResult::Success && tracking
    }

    // ── Terminal data ──────────────────────────────────────────────────

    /// Current terminal width in cells.
    pub fn cols(&self) -> u16 {
        let mut cols: u16 = 0;
        unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::Cols,
                &mut cols as *mut u16 as *mut c_void,
            );
        }
        cols
    }

    /// Current terminal height in cells.
    pub fn rows(&self) -> u16 {
        let mut rows: u16 = 0;
        unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::Rows,
                &mut rows as *mut u16 as *mut c_void,
            );
        }
        rows
    }

    /// Cursor position (x=col, y=row), both 0-indexed.
    pub fn cursor_position(&self) -> (u16, u16) {
        let mut x: u16 = 0;
        let mut y: u16 = 0;
        unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::CursorX,
                &mut x as *mut u16 as *mut c_void,
            );
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::CursorY,
                &mut y as *mut u16 as *mut c_void,
            );
        }
        (x, y)
    }

    /// Number of scrollback rows.
    pub fn scrollback_rows(&self) -> usize {
        let mut rows: usize = 0;
        unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::ScrollbackRows,
                &mut rows as *mut usize as *mut c_void,
            );
        }
        rows
    }

    /// Scrollbar geometry for the current viewport.
    pub fn scrollbar(&self) -> ScrollbarState {
        let mut scrollbar = GhosttyTerminalScrollbar::default();
        let result = unsafe {
            ghostty_terminal_get(
                self.handle,
                GhosttyTerminalData::Scrollbar,
                &mut scrollbar as *mut GhosttyTerminalScrollbar as *mut c_void,
            )
        };

        if result != GhosttyResult::Success {
            return ScrollbarState::default();
        }

        ScrollbarState {
            total: scrollbar.total as usize,
            offset: scrollbar.offset as usize,
            len: scrollbar.len as usize,
        }
    }

    /// Terminal title as set by OSC 0/2.
    pub fn title(&self) -> String {
        self.get_string(GhosttyTerminalData::Title)
    }

    /// Current working directory as set by OSC 7.
    pub fn pwd(&self) -> String {
        self.get_string(GhosttyTerminalData::Pwd)
    }

    /// Effective foreground color (override or default), if set.
    pub fn foreground_color(&self) -> Option<GhosttyColorRgb> {
        self.get_color(GhosttyTerminalData::ColorForeground)
    }

    /// Effective background color (override or default), if set.
    pub fn background_color(&self) -> Option<GhosttyColorRgb> {
        self.get_color(GhosttyTerminalData::ColorBackground)
    }

    /// Default foreground color, ignoring OSC overrides, if set.
    pub fn foreground_color_default(&self) -> Option<GhosttyColorRgb> {
        self.get_color(GhosttyTerminalData::ColorForegroundDefault)
    }

    /// Default background color, ignoring OSC overrides, if set.
    pub fn background_color_default(&self) -> Option<GhosttyColorRgb> {
        self.get_color(GhosttyTerminalData::ColorBackgroundDefault)
    }

    /// Effective cursor color (override or default), if set.
    pub fn cursor_color(&self) -> Option<GhosttyColorRgb> {
        self.get_color(GhosttyTerminalData::ColorCursor)
    }

    /// Default cursor color, ignoring OSC overrides, if set.
    pub fn cursor_color_default(&self) -> Option<GhosttyColorRgb> {
        self.get_color(GhosttyTerminalData::ColorCursorDefault)
    }

    /// Read a string data field from the terminal.
    fn get_string(&self, data: GhosttyTerminalData) -> String {
        let mut s = GhosttyString {
            ptr: ptr::null(),
            len: 0,
        };
        let result = unsafe {
            ghostty_terminal_get(
                self.handle,
                data,
                &mut s as *mut GhosttyString as *mut c_void,
            )
        };
        if result != GhosttyResult::Success || s.ptr.is_null() || s.len == 0 {
            return String::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(s.ptr, s.len) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Read a color data field from the terminal.
    fn get_color(&self, data: GhosttyTerminalData) -> Option<GhosttyColorRgb> {
        let mut color = GhosttyColorRgb { r: 0, g: 0, b: 0 };
        let result = unsafe {
            ghostty_terminal_get(
                self.handle,
                data,
                &mut color as *mut GhosttyColorRgb as *mut c_void,
            )
        };
        (result == GhosttyResult::Success).then_some(color)
    }

    // ── Effect callbacks ───────────────────────────────────────────────

    /// Set the userdata pointer passed to all callbacks.
    ///
    /// # Safety
    /// Caller must ensure the pointer remains valid for the lifetime of this terminal.
    pub unsafe fn set_userdata(&mut self, userdata: *mut c_void) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::Userdata,
                userdata as *const c_void,
            );
        }
    }

    /// Set the write_pty callback (DSR/DA responses written back to PTY).
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_write_pty_callback(&mut self, cb: GhosttyTerminalWritePtyFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::WritePty,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the bell callback.
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_bell_callback(&mut self, cb: GhosttyTerminalBellFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::Bell,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the color-scheme callback (CSI ? 996 n).
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_color_scheme_callback(&mut self, cb: GhosttyTerminalColorSchemeFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::ColorScheme,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Answer Ghostty color-scheme queries from the terminal's default background.
    pub unsafe fn enable_builtin_color_scheme_callback(&mut self) {
        unsafe {
            self.set_color_scheme_callback(Some(builtin_color_scheme_trampoline));
        }
    }

    /// Set the title_changed callback.
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_title_changed_callback(&mut self, cb: GhosttyTerminalTitleChangedFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::TitleChanged,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the pwd_changed callback (OSC 7).
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_pwd_changed_callback(&mut self, cb: GhosttyTerminalPwdChangedFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::PwdChanged,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the notification callback (OSC 9/777).
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_notification_callback(&mut self, cb: GhosttyTerminalNotificationFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::Notification,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the semantic_prompt callback (OSC 133).
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_semantic_prompt_callback(&mut self, cb: GhosttyTerminalSemanticPromptFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::SemanticPrompt,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the mode_changed callback (CSI h/l).
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_mode_changed_callback(&mut self, cb: GhosttyTerminalModeChangedFn) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::ModeChanged,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    /// Set the kitty_keyboard_changed callback.
    ///
    /// # Safety
    /// The function pointer must be valid and the userdata must be set.
    pub unsafe fn set_kitty_keyboard_changed_callback(
        &mut self,
        cb: GhosttyTerminalKittyKeyboardChangedFn,
    ) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::KittyKeyboardChanged,
                cb.map_or(ptr::null(), |f| f as *const c_void),
            );
        }
    }

    // ── Color configuration ────────────────────────────────────────────

    /// Set the default foreground color.
    pub fn set_color_foreground(&mut self, color: GhosttyColorRgb) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::ColorForeground,
                &color as *const GhosttyColorRgb as *const c_void,
            );
        }
    }

    /// Set the default background color.
    pub fn set_color_background(&mut self, color: GhosttyColorRgb) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::ColorBackground,
                &color as *const GhosttyColorRgb as *const c_void,
            );
        }
    }

    /// Set the default cursor color.
    pub fn set_color_cursor(&mut self, color: GhosttyColorRgb) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::ColorCursor,
                &color as *const GhosttyColorRgb as *const c_void,
            );
        }
    }

    /// Set the default 256-color palette.
    pub fn set_color_palette(&mut self, palette: &[GhosttyColorRgb; 256]) {
        unsafe {
            ghostty_terminal_set(
                self.handle,
                GhosttyTerminalOption::ColorPalette,
                palette.as_ptr() as *const c_void,
            );
        }
    }

    // ── Viewport scrolling ─────────────────────────────────────────────

    /// Scroll the viewport by delta rows (negative = up, positive = down).
    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        let behavior = GhosttyScrollViewport {
            tag: GhosttyScrollViewportTag::Delta,
            value: GhosttyScrollViewportValue { delta },
        };
        unsafe { ghostty_terminal_scroll_viewport(self.handle, behavior) };
    }

    /// Scroll the viewport to the bottom (live view).
    pub fn scroll_viewport_bottom(&mut self) {
        let behavior = GhosttyScrollViewport {
            tag: GhosttyScrollViewportTag::Bottom,
            value: GhosttyScrollViewportValue { _padding: [0; 2] },
        };
        unsafe { ghostty_terminal_scroll_viewport(self.handle, behavior) };
    }

    /// Scroll the viewport to the top.
    pub fn scroll_viewport_top(&mut self) {
        let behavior = GhosttyScrollViewport {
            tag: GhosttyScrollViewportTag::Top,
            value: GhosttyScrollViewportValue { _padding: [0; 2] },
        };
        unsafe { ghostty_terminal_scroll_viewport(self.handle, behavior) };
    }

    // ── Grid ref (non-render-loop cell access) ─────────────────────────

    /// Look up a cell at the given active-area coordinate.
    pub fn grid_ref_active(&self, x: u16, y: u32) -> Option<GhosttyGridRef> {
        let point = GhosttyPoint {
            tag: GhosttyPointTag::Active,
            value: GhosttyPointValue {
                coordinate: GhosttyPointCoordinate { x, y },
            },
        };
        let mut gref = GhosttyGridRef::new_sized();
        let result = unsafe { ghostty_terminal_grid_ref(self.handle, point, &mut gref) };
        if result == GhosttyResult::Success {
            Some(gref)
        } else {
            None
        }
    }

    /// Look up a cell at the given viewport coordinate.
    pub fn grid_ref_viewport(&self, x: u16, y: u32) -> Option<GhosttyGridRef> {
        let point = GhosttyPoint {
            tag: GhosttyPointTag::Viewport,
            value: GhosttyPointValue {
                coordinate: GhosttyPointCoordinate { x, y },
            },
        };
        let mut gref = GhosttyGridRef::new_sized();
        let result = unsafe { ghostty_terminal_grid_ref(self.handle, point, &mut gref) };
        if result == GhosttyResult::Success {
            Some(gref)
        } else {
            None
        }
    }

    // ── Formatting ─────────────────────────────────────────────────────

    /// Format the terminal's active screen as VT sequences (ANSI escape codes).
    pub fn format_vt(&self) -> Result<Vec<u8>, &'static str> {
        self.format(GhosttyFormatterFormat::Vt, false, false)
    }

    /// Format the terminal's active screen as plain text (no escape sequences).
    pub fn format_plain(&self) -> Result<Vec<u8>, &'static str> {
        self.format(GhosttyFormatterFormat::Plain, false, true)
    }

    fn format(
        &self,
        emit: GhosttyFormatterFormat,
        unwrap: bool,
        trim: bool,
    ) -> Result<Vec<u8>, &'static str> {
        let screen_extra = GhosttyFormatterScreenExtra {
            size: std::mem::size_of::<GhosttyFormatterScreenExtra>(),
            cursor: false,
            style: false,
            hyperlink: false,
            protection: false,
            kitty_keyboard: false,
            charsets: false,
        };
        let terminal_extra = GhosttyFormatterTerminalExtra {
            size: std::mem::size_of::<GhosttyFormatterTerminalExtra>(),
            palette: false,
            modes: false,
            scrolling_region: false,
            tabstops: false,
            pwd: false,
            keyboard: false,
            screen: screen_extra,
        };
        let opts = GhosttyFormatterTerminalOptions {
            size: std::mem::size_of::<GhosttyFormatterTerminalOptions>(),
            emit,
            unwrap,
            trim,
            extra: terminal_extra,
        };

        self.format_with_opts(opts)
    }

    // ── Opaque terminal snapshot transfer ───────────────────────────────

    /// Export the entire terminal state as one opaque blob.
    pub fn snapshot_export(&self) -> Option<Vec<u8>> {
        let mut ptr: *mut u8 = ptr::null_mut();
        let mut len: usize = 0;
        let result = unsafe {
            ghostty_terminal_snapshot_export(self.handle, ptr::null(), &mut ptr, &mut len)
        };
        if result == GhosttyResult::Success && !ptr.is_null() && len > 0 {
            let data = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
            unsafe { ghostty_free(ptr::null(), ptr, len) };
            Some(data)
        } else {
            None
        }
    }

    /// Import an entire terminal state from an opaque blob produced by
    /// `snapshot_export()`.
    pub fn snapshot_import(&mut self, data: &[u8]) -> Result<(), &'static str> {
        let result =
            unsafe { ghostty_terminal_snapshot_import(self.handle, data.as_ptr(), data.len()) };
        match result {
            GhosttyResult::Success => Ok(()),
            _ => Err("ghostty_terminal_snapshot_import: failed"),
        }
    }

    fn format_with_opts(
        &self,
        opts: GhosttyFormatterTerminalOptions,
    ) -> Result<Vec<u8>, &'static str> {
        let mut formatter: GhosttyFormatterPtr = ptr::null_mut();

        let result = unsafe {
            ghostty_formatter_terminal_new(ptr::null(), &mut formatter, self.handle, opts)
        };
        if result != GhosttyResult::Success {
            return Err("ghostty_formatter_terminal_new: failed");
        }

        let mut out_ptr: *mut u8 = ptr::null_mut();
        let mut out_len: usize = 0;

        let result = unsafe {
            ghostty_formatter_format_alloc(formatter, ptr::null(), &mut out_ptr, &mut out_len)
        };

        if result != GhosttyResult::Success {
            unsafe { ghostty_formatter_free(formatter) };
            return Err("ghostty_formatter_format_alloc: failed");
        }

        let output = if !out_ptr.is_null() && out_len > 0 {
            unsafe { std::slice::from_raw_parts(out_ptr, out_len) }.to_vec()
        } else {
            Vec::new()
        };

        if !out_ptr.is_null() {
            unsafe { ghostty_free(ptr::null(), out_ptr, out_len) };
        }
        unsafe { ghostty_formatter_free(formatter) };

        Ok(output)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ghostty_terminal_free(self.handle) };
        }
    }
}

// ---------------------------------------------------------------------------
// Render state wrapper
// ---------------------------------------------------------------------------

/// Ghostty render state snapshot.
///
/// Immutable after `update()`. Query cursor, colors, dimensions.
/// Create a `RenderIterator` to iterate rows and cells.
pub struct RenderState {
    handle: GhosttyRenderStatePtr,
}

impl std::fmt::Debug for RenderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderState").finish_non_exhaustive()
    }
}

unsafe impl Send for RenderState {}

impl RenderState {
    /// Create a new render state. Call `update()` to populate from a terminal.
    pub fn new() -> Result<Self, &'static str> {
        let mut handle: GhosttyRenderStatePtr = ptr::null_mut();
        let result = unsafe { ghostty_render_state_new(ptr::null(), &mut handle) };
        if result != GhosttyResult::Success {
            return Err("ghostty_render_state_new: failed");
        }
        Ok(RenderState { handle })
    }

    /// Update from a terminal. Call this before reading render data.
    pub fn update(&mut self, terminal: &mut Terminal) -> Result<(), &'static str> {
        let result = unsafe { ghostty_render_state_update(self.handle, terminal.handle()) };
        match result {
            GhosttyResult::Success => Ok(()),
            _ => Err("ghostty_render_state_update: failed"),
        }
    }

    pub(crate) fn handle(&self) -> GhosttyRenderStatePtr {
        self.handle
    }

    /// Get the viewport dimensions.
    pub fn dimensions(&self) -> (u16, u16) {
        let mut cols: u16 = 0;
        let mut rows: u16 = 0;
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::Cols,
                &mut cols as *mut u16 as *mut c_void,
            );
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::Rows,
                &mut rows as *mut u16 as *mut c_void,
            );
        }
        (cols, rows)
    }

    /// Cursor visual style.
    pub fn cursor_visual_style(&self) -> GhosttyRenderStateCursorVisualStyle {
        let mut style = GhosttyRenderStateCursorVisualStyle::Block;
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::CursorVisualStyle,
                &mut style as *mut GhosttyRenderStateCursorVisualStyle as *mut c_void,
            );
        }
        style
    }

    /// Whether the cursor is visible.
    pub fn cursor_visible(&self) -> bool {
        let mut visible = false;
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::CursorVisible,
                &mut visible as *mut bool as *mut c_void,
            );
        }
        visible
    }

    /// Whether the cursor is in the viewport.
    pub fn cursor_in_viewport(&self) -> bool {
        let mut has_value = false;
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::CursorViewportHasValue,
                &mut has_value as *mut bool as *mut c_void,
            );
        }
        has_value
    }

    /// Cursor viewport position (x, y). Only valid when `cursor_in_viewport()`.
    pub fn cursor_viewport_position(&self) -> (u16, u16) {
        let mut x: u16 = 0;
        let mut y: u16 = 0;
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::CursorViewportX,
                &mut x as *mut u16 as *mut c_void,
            );
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::CursorViewportY,
                &mut y as *mut u16 as *mut c_void,
            );
        }
        (x, y)
    }

    /// Default foreground color.
    pub fn foreground_color(&self) -> GhosttyColorRgb {
        let mut color = GhosttyColorRgb {
            r: 255,
            g: 255,
            b: 255,
        };
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::ColorForeground,
                &mut color as *mut GhosttyColorRgb as *mut c_void,
            );
        }
        color
    }

    /// Default background color.
    pub fn background_color(&self) -> GhosttyColorRgb {
        let mut color = GhosttyColorRgb { r: 0, g: 0, b: 0 };
        unsafe {
            ghostty_render_state_get(
                self.handle,
                GhosttyRenderStateData::ColorBackground,
                &mut color as *mut GhosttyColorRgb as *mut c_void,
            );
        }
        color
    }

    /// Create a `RenderIterator` for iterating rows and cells.
    pub fn iterator(&self) -> Result<RenderIterator, &'static str> {
        RenderIterator::new(self)
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ghostty_render_state_free(self.handle) };
        }
    }
}

/// Mutable iterator over render state rows and cells.
///
/// Created per render frame from `RenderState::iterator()`. Owns the
/// ghostty row iterator and row cells handles.
pub struct RenderIterator {
    row_iterator: GhosttyRowIteratorPtr,
    row_cells: GhosttyRowCellsPtr,
}

impl RenderIterator {
    fn new(rs: &RenderState) -> Result<Self, &'static str> {
        let mut row_iterator: GhosttyRowIteratorPtr = ptr::null_mut();
        let result =
            unsafe { ghostty_render_state_row_iterator_new(ptr::null(), &mut row_iterator) };
        if result != GhosttyResult::Success {
            return Err("ghostty_render_state_row_iterator_new: failed");
        }

        let mut row_cells: GhosttyRowCellsPtr = ptr::null_mut();
        let result = unsafe { ghostty_render_state_row_cells_new(ptr::null(), &mut row_cells) };
        if result != GhosttyResult::Success {
            unsafe { ghostty_render_state_row_iterator_free(row_iterator) };
            return Err("ghostty_render_state_row_cells_new: failed");
        }

        // Populate the row iterator from the render state.
        // Pass &row_iterator (pointer to the handle) per ghostling reference.
        unsafe {
            ghostty_render_state_get(
                rs.handle(),
                GhosttyRenderStateData::RowIterator,
                &mut row_iterator as *mut GhosttyRowIteratorPtr as *mut c_void,
            );
        }

        Ok(RenderIterator {
            row_iterator,
            row_cells,
        })
    }

    /// Advance to the next row. Returns false when past the last row.
    pub fn next_row(&mut self) -> bool {
        unsafe { ghostty_render_state_row_iterator_next(self.row_iterator) }
    }

    /// Get the raw GhosttyRow for the current row.
    pub fn current_row(&self) -> GhosttyRow {
        let mut row: GhosttyRow = 0;
        unsafe {
            ghostty_render_state_row_get(
                self.row_iterator,
                GhosttyRenderStateRowData::Raw,
                &mut row as *mut GhosttyRow as *mut c_void,
            );
        }
        row
    }

    /// Populate cells for the current row. Call before iterating cells.
    pub fn begin_cells(&mut self) {
        unsafe {
            ghostty_render_state_row_get(
                self.row_iterator,
                GhosttyRenderStateRowData::Cells,
                &mut self.row_cells as *mut GhosttyRowCellsPtr as *mut c_void,
            );
        }
    }

    /// Advance to the next cell. Returns false when past the last cell.
    pub fn next_cell(&mut self) -> bool {
        unsafe { ghostty_render_state_row_cells_next(self.row_cells) }
    }

    /// Get the raw GhosttyCell for the current cell.
    pub fn current_cell(&self) -> GhosttyCell {
        let mut cell: GhosttyCell = 0;
        unsafe {
            ghostty_render_state_row_cells_get(
                self.row_cells,
                GhosttyRenderStateRowCellsData::Raw,
                &mut cell as *mut GhosttyCell as *mut c_void,
            );
        }
        cell
    }

    /// Get the style for the current cell.
    pub fn current_cell_style(&self) -> GhosttyStyle {
        let mut style = GhosttyStyle::default_sized();
        unsafe {
            ghostty_render_state_row_cells_get(
                self.row_cells,
                GhosttyRenderStateRowCellsData::Style,
                &mut style as *mut GhosttyStyle as *mut c_void,
            );
        }
        style
    }

    /// Get grapheme codepoints for the current cell.
    pub fn current_cell_graphemes(&self) -> Vec<char> {
        let mut len: u32 = 0;
        unsafe {
            ghostty_render_state_row_cells_get(
                self.row_cells,
                GhosttyRenderStateRowCellsData::GraphemesLen,
                &mut len as *mut u32 as *mut c_void,
            );
        }
        if len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u32; len as usize];
        unsafe {
            ghostty_render_state_row_cells_get(
                self.row_cells,
                GhosttyRenderStateRowCellsData::GraphemesBuf,
                buf.as_mut_ptr() as *mut c_void,
            );
        }
        buf.into_iter()
            .filter_map(|cp| char::from_u32(cp))
            .collect()
    }

    /// Get the resolved foreground color for the current cell.
    pub fn current_cell_fg(&self) -> Option<GhosttyColorRgb> {
        let mut color = GhosttyColorRgb { r: 0, g: 0, b: 0 };
        let result = unsafe {
            ghostty_render_state_row_cells_get(
                self.row_cells,
                GhosttyRenderStateRowCellsData::FgColor,
                &mut color as *mut GhosttyColorRgb as *mut c_void,
            )
        };
        if result == GhosttyResult::Success {
            Some(color)
        } else {
            None
        }
    }

    /// Get the resolved background color for the current cell.
    pub fn current_cell_bg(&self) -> Option<GhosttyColorRgb> {
        let mut color = GhosttyColorRgb { r: 0, g: 0, b: 0 };
        let result = unsafe {
            ghostty_render_state_row_cells_get(
                self.row_cells,
                GhosttyRenderStateRowCellsData::BgColor,
                &mut color as *mut GhosttyColorRgb as *mut c_void,
            )
        };
        if result == GhosttyResult::Success {
            Some(color)
        } else {
            None
        }
    }
}

impl Drop for RenderIterator {
    fn drop(&mut self) {
        if !self.row_cells.is_null() {
            unsafe { ghostty_render_state_row_cells_free(self.row_cells) };
        }
        if !self.row_iterator.is_null() {
            unsafe { ghostty_render_state_row_iterator_free(self.row_iterator) };
        }
    }
}

// ---------------------------------------------------------------------------
// Cell/Row helpers (used with grid_ref or render state raw values)
// ---------------------------------------------------------------------------

/// Get the codepoint from a cell.
pub fn cell_codepoint(cell: GhosttyCell) -> u32 {
    let mut cp: u32 = 0;
    unsafe {
        ghostty_cell_get(
            cell,
            GhosttyCellData::Codepoint,
            &mut cp as *mut u32 as *mut c_void,
        );
    }
    cp
}

/// Get the wide property of a cell.
pub fn cell_wide(cell: GhosttyCell) -> GhosttyCellWide {
    let mut wide = GhosttyCellWide::Narrow;
    unsafe {
        ghostty_cell_get(
            cell,
            GhosttyCellData::Wide,
            &mut wide as *mut GhosttyCellWide as *mut c_void,
        );
    }
    wide
}

/// Check if a cell has text to render.
pub fn cell_has_text(cell: GhosttyCell) -> bool {
    let mut has = false;
    unsafe {
        ghostty_cell_get(
            cell,
            GhosttyCellData::HasText,
            &mut has as *mut bool as *mut c_void,
        );
    }
    has
}

/// Check if a row is soft-wrapped.
pub fn row_wraps(row: GhosttyRow) -> bool {
    let mut wraps = false;
    unsafe {
        ghostty_row_get(
            row,
            GhosttyRowData::Wrap,
            &mut wraps as *mut bool as *mut c_void,
        );
    }
    wraps
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_write_format_roundtrip() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");

        term.write(b"hello world");

        let plain = term.format_plain().expect("format_plain failed");
        let plain_str = String::from_utf8_lossy(&plain);
        assert!(
            plain_str.contains("hello world"),
            "plain output should contain 'hello world', got: {:?}",
            plain_str
        );

        let vt = term.format_vt().expect("format_vt failed");
        let vt_str = String::from_utf8_lossy(&vt);
        assert!(
            vt_str.contains("hello world"),
            "VT output should contain 'hello world', got: {:?}",
            vt_str
        );
    }

    #[test]
    fn styled_text_produces_sgr_in_vt_format() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");

        term.write(b"\x1b[1mbold text\x1b[0m");

        let vt = term.format_vt().expect("format_vt failed");
        let vt_str = String::from_utf8_lossy(&vt);
        assert!(vt_str.contains("bold text"));
        assert!(vt_str.contains("\x1b["));
    }

    #[test]
    fn resize_works() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");
        term.resize(120, 40).expect("resize failed");

        term.write(b"after resize");
        let plain = term.format_plain().expect("format_plain failed");
        assert!(String::from_utf8_lossy(&plain).contains("after resize"));
    }

    #[test]
    fn drop_is_safe() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");
        term.write(b"will be dropped");
    }

    #[test]
    fn mode_queries_work() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");

        // Default state: cursor visible, no alt screen, no bracketed paste
        assert!(!term.cursor_hidden(), "cursor should be visible by default");
        assert!(
            !term.alt_screen_active(),
            "alt screen should be off by default"
        );
        assert!(
            !term.bracketed_paste(),
            "bracketed paste should be off by default"
        );
        assert!(!term.kitty_enabled(), "kitty should be off by default");
        assert!(
            !term.focus_reporting(),
            "focus reporting should be off by default"
        );

        // Enable bracketed paste
        term.write(b"\x1b[?2004h");
        assert!(
            term.bracketed_paste(),
            "bracketed paste should be on after CSI ?2004h"
        );

        // Enter alt screen
        term.write(b"\x1b[?1049h");
        assert!(
            term.alt_screen_active(),
            "alt screen should be on after CSI ?1049h"
        );

        // Hide cursor
        term.write(b"\x1b[?25l");
        assert!(
            term.cursor_hidden(),
            "cursor should be hidden after CSI ?25l"
        );
    }

    #[test]
    fn terminal_data_queries_work() {
        let term = Terminal::new(80, 24, 0).expect("terminal creation failed");

        assert_eq!(term.cols(), 80);
        assert_eq!(term.rows(), 24);
        assert_eq!(term.scrollback_rows(), 0);
        assert_eq!(term.cursor_position(), (0, 0));
    }

    #[test]
    fn mouse_mode_bitmask() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");

        assert_eq!(term.mouse_mode(), 0, "no mouse tracking by default");

        // Enable normal mouse tracking
        term.write(b"\x1b[?1000h");
        assert_ne!(
            term.mouse_mode(),
            0,
            "mouse mode should be non-zero after enable"
        );
        assert!(term.mouse_tracking(), "mouse_tracking should be true");
    }

    #[test]
    fn render_state_basic() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");
        term.write(b"render test");

        let mut rs = RenderState::new().expect("render state creation failed");
        rs.update(&mut term).expect("render state update failed");

        let (cols, rows) = rs.dimensions();
        assert_eq!(cols, 80);
        assert_eq!(rows, 24);
    }

    #[test]
    fn render_state_re_resolves_palette_backed_cells_after_palette_change() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");

        let mut palette = [GhosttyColorRgb { r: 0, g: 0, b: 0 }; 256];
        palette[4] = GhosttyColorRgb {
            r: 0xF0,
            g: 0xE0,
            b: 0xD0,
        };
        term.set_color_palette(&palette);
        term.write(b"\x1b[44mX\x1b[0m");

        let mut rs = RenderState::new().expect("render state creation failed");
        rs.update(&mut term).expect("render state update failed");

        let mut iter = rs.iterator().expect("iterator");
        assert!(iter.next_row(), "first row");
        iter.begin_cells();
        assert!(iter.next_cell(), "first cell");
        let initial_style = iter.current_cell_style();
        let initial_bg = iter.current_cell_bg().expect("initial bg");

        assert_eq!(initial_style.bg_color.tag, GhosttyStyleColorTag::Palette);
        let initial_palette_index = unsafe { initial_style.bg_color.value.palette };
        assert_eq!(initial_palette_index, 4);
        assert_eq!(
            initial_bg,
            GhosttyColorRgb {
                r: 0xF0,
                g: 0xE0,
                b: 0xD0
            }
        );

        palette[4] = GhosttyColorRgb {
            r: 0x10,
            g: 0x0F,
            b: 0x0F,
        };
        term.set_color_palette(&palette);
        rs.update(&mut term).expect("render state update failed");

        let mut iter = rs.iterator().expect("iterator");
        assert!(iter.next_row(), "first row");
        iter.begin_cells();
        assert!(iter.next_cell(), "first cell");
        let updated_style = iter.current_cell_style();
        let updated_bg = iter.current_cell_bg().expect("updated bg");

        assert_eq!(updated_style.bg_color.tag, GhosttyStyleColorTag::Palette);
        let updated_palette_index = unsafe { updated_style.bg_color.value.palette };
        assert_eq!(updated_palette_index, 4);
        assert_eq!(
            updated_bg,
            GhosttyColorRgb {
                r: 0x10,
                g: 0x0F,
                b: 0x0F
            }
        );
    }

    #[test]
    fn title_query() {
        let mut term = Terminal::new(80, 24, 0).expect("terminal creation failed");
        assert_eq!(term.title(), "");

        // Set title via OSC 2 — requires title_changed callback for ghostty
        // to process it. Without callback, ghostty still updates internal state.
        term.write(b"\x1b]2;My Title\x07");
        assert_eq!(term.title(), "My Title");
    }
}
