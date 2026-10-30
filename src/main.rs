use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const APP_ID: &str = "com.example.Notes";
const SAVE_DELAY: Duration = Duration::from_millis(500);

// ───────────────────────── Data model ─────────────────────────

struct Note {
    id: String,
    content: String,
    dirty: bool,           // has unsaved changes
    modified: SystemTime,  // last edit time (shown in the sidebar)
    pinned: bool,          // pinned notes stay at the top of the list
}

impl Note {
    fn new() -> Self {
        Note {
            id: new_id(),
            content: String::new(),
            dirty: false,
            modified: SystemTime::now(),
            pinned: false,
        }
    }

    fn path(&self) -> PathBuf {
        notes_dir().join(format!("{}.md", self.id))
    }

    /// Title = first non-empty line, without leading '#' characters.
    fn title(&self) -> String {
        let first = self
            .content
            .lines()
            .find(|l| !l.trim().is_empty() && !is_tag_line(l))
            .or_else(|| self.content.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or("");
        let t = first.trim().trim_start_matches('#').trim();
        if t.is_empty() {
            "Untitled note".to_string()
        } else {
            t.to_string()
        }
    }

    /// Subtitle = last-edited time + a one-line preview of the body.
    fn subtitle(&self) -> String {
        if self.content.trim().is_empty() {
            return "Empty note".to_string();
        }
        let when = format_time(self.modified);
        let preview = self
            .content
            .lines()
            .filter(|l| !l.trim().is_empty() && !is_tag_line(l))
            .nth(1)
            .map(|l| {
                l.trim()
                    .trim_start_matches(|c: char| matches!(c, '#' | '-' | '*' | '>' | ' '))
                    .to_string()
            })
            .unwrap_or_default();
        if preview.is_empty() {
            when
        } else {
            format!("{when} · {preview}")
        }
    }

    /// Returns true when the note reached the disk.
    fn save(&self) -> bool {
        match write_atomic(&self.path(), &self.content) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("Failed to save note {}: {e}", self.id);
                false
            }
        }
    }
}

struct State {
    notes: Vec<Note>,       // same order as the sidebar rows
    current: Option<usize>, // index of the note shown in the editor
    loading: bool,          // true while we set the editor text from code
    save_pending: bool,     // a save timer is already scheduled
}

impl State {
    fn save_dirty(&mut self) {
        for note in self.notes.iter_mut().filter(|n| n.dirty) {
            if note.save() {
                note.dirty = false;
            }
        }
    }
}

// ───────────────────────── Storage helpers ─────────────────────────

fn notes_dir() -> PathBuf {
    let dir = glib::user_data_dir().join("notes-app").join("notes");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Writes to a temporary file first and then renames it over the real file, so a
/// crash or power cut can never leave a half-written note behind.
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn pins_path() -> PathBuf {
    glib::user_data_dir().join("notes-app").join("pinned.txt")
}

fn load_pins() -> HashSet<String> {
    fs::read_to_string(pins_path())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Remembers which notes are pinned (one note id per line).
fn save_pins(notes: &[Note]) {
    let text: String = notes
        .iter()
        .filter(|n| n.pinned)
        .map(|n| format!("{}\n", n.id))
        .collect();
    if let Err(e) = write_atomic(&pins_path(), &text) {
        eprintln!("Failed to save pins: {e}");
    }
}

fn theme_path() -> PathBuf {
    glib::user_data_dir().join("notes-app").join("theme.txt")
}

/// "system" (default), "light" or "dark".
fn load_theme() -> String {
    match fs::read_to_string(theme_path()).map(|t| t.trim().to_string()) {
        Ok(t) if t == "light" || t == "dark" => t,
        _ => "system".to_string(),
    }
}

fn save_theme(theme: &str) {
    let _ = write_atomic(&theme_path(), theme);
}

fn apply_theme(theme: &str) {
    let scheme = match theme {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// "Just now", "14:32" (today) or "Oct 30" (older).
fn format_time(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (Ok(then), Ok(now)) = (glib::DateTime::from_unix_local(secs), glib::DateTime::now_local())
    else {
        return String::new();
    };
    if now.to_unix() - secs < 60 {
        return "Just now".to_string();
    }
    let day = |d: &glib::DateTime| d.format("%Y-%m-%d").map(|g| g.to_string()).unwrap_or_default();
    let fmt = if day(&then) == day(&now) { "%H:%M" } else { "%b %-d" };
    then.format(fmt).map(|g| g.to_string()).unwrap_or_default()
}

fn new_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string()
}

/// Loads all notes: pinned first, then most recently modified first.
fn load_notes() -> Vec<Note> {
    let pins = load_pins();
    let mut items: Vec<(SystemTime, Note)> = Vec::new();

    if let Ok(entries) = fs::read_dir(notes_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            let pinned = pins.contains(&id);
            items.push((modified, Note { id, content, dirty: false, modified, pinned }));
        }
    }

    items.sort_by(|a, b| b.1.pinned.cmp(&a.1.pinned).then(b.0.cmp(&a.0)));
    items.into_iter().map(|(_, n)| n).collect()
}

// ───────────────────────── Tags ─────────────────────────

/// Finds #tags in a line: returns (start, end) character ranges, '#' included.
/// A tag starts at a word boundary and begins with a letter ("# Title" is not a tag).
fn find_tags(chars: &[char], start: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i < chars.len() {
        let at_boundary = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '#' && at_boundary && i + 1 < chars.len() && chars[i + 1].is_alphabetic() {
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '-')
            {
                j += 1;
            }
            out.push((i, j));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// True for a line that contains nothing but tags, like "#work #ideas".
fn is_tag_line(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let tags = find_tags(&chars, 0);
    !tags.is_empty() && line.split_whitespace().count() == tags.len()
}

/// All tags in a note (lowercase, sorted, no duplicates). Code blocks are ignored.
fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = BTreeSet::new();
    let mut in_code = false;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for (a, b) in find_tags(&chars, 0) {
            tags.insert(chars[a..b].iter().collect::<String>().to_lowercase());
        }
    }
    tags.into_iter().collect()
}

fn all_tags(state: &State) -> Vec<String> {
    let mut set = BTreeSet::new();
    for note in &state.notes {
        set.extend(extract_tags(&note.content));
    }
    set.into_iter().collect()
}

/// The row of clickable tag chips under the search box.
struct TagUi {
    scroller: gtk::ScrolledWindow,
    bar: gtk::Box,
    list: gtk::ListBox,
    filter: Rc<RefCell<Option<String>>>, // the tag currently filtering the list
    shown: RefCell<Vec<String>>,         // tags currently displayed as chips
}

/// Rebuilds the chips, but only when the set of tags actually changed.
fn refresh_tags(state: &Rc<RefCell<State>>, ui: &TagUi) {
    let tags = all_tags(&state.borrow());
    if *ui.shown.borrow() == tags {
        return;
    }
    *ui.shown.borrow_mut() = tags.clone();

    // If the tag we were filtering by no longer exists, stop filtering
    let stale = matches!(&*ui.filter.borrow(), Some(t) if !tags.contains(t));
    if stale {
        *ui.filter.borrow_mut() = None;
        ui.list.invalidate_filter();
    }

    while let Some(child) = ui.bar.first_child() {
        ui.bar.remove(&child);
    }
    ui.scroller.set_visible(!tags.is_empty());

    for tag in tags {
        let chip = gtk::ToggleButton::builder()
            .label(&tag)
            .css_classes(["flat"])
            .build();
        chip.set_active(ui.filter.borrow().as_deref() == Some(tag.as_str()));

        let (filter, list, bar) = (ui.filter.clone(), ui.list.clone(), ui.bar.clone());
        chip.connect_toggled(move |btn| {
            if btn.is_active() {
                *filter.borrow_mut() = Some(tag.clone());
                // only one chip can be active at a time
                let mut child = bar.first_child();
                while let Some(c) = child {
                    if let Some(other) = c.downcast_ref::<gtk::ToggleButton>() {
                        if other != btn && other.is_active() {
                            other.set_active(false);
                        }
                    }
                    child = c.next_sibling();
                }
                list.invalidate_filter();
            } else if filter.borrow().as_deref() == Some(tag.as_str()) {
                *filter.borrow_mut() = None;
                list.invalidate_filter();
            }
        });
        ui.bar.append(&chip);
    }
}

// ───────────────────────── Markdown styling ─────────────────────────

/// Creates the text styles used for Markdown. Tags created later win
/// over earlier ones, so "marker" (the dimmed symbols) goes last.
fn create_tags(buffer: &gtk::TextBuffer) {
    buffer.create_tag(Some("h1"), &[("scale", &1.7_f64), ("weight", &700_i32)]);
    buffer.create_tag(Some("h2"), &[("scale", &1.4_f64), ("weight", &700_i32)]);
    buffer.create_tag(Some("h3"), &[("scale", &1.2_f64), ("weight", &700_i32)]);
    buffer.create_tag(Some("bold"), &[("weight", &700_i32)]);
    buffer.create_tag(Some("italic"), &[("style", &pango::Style::Italic)]);
    buffer.create_tag(
        Some("code"),
        &[
            ("family", &"monospace"),
            ("background-rgba", &gdk::RGBA::new(0.5, 0.5, 0.5, 0.2)),
        ],
    );
    buffer.create_tag(
        Some("quote"),
        &[
            ("style", &pango::Style::Italic),
            ("foreground-rgba", &gdk::RGBA::new(0.5, 0.5, 0.5, 1.0)),
        ],
    );
    buffer.create_tag(Some("bullet"), &[("weight", &700_i32)]);
    buffer.create_tag(
        Some("tag"),
        &[("foreground-rgba", &gdk::RGBA::new(0.25, 0.55, 0.95, 1.0))],
    );
    buffer.create_tag(
        Some("marker"),
        &[("foreground-rgba", &gdk::RGBA::new(0.5, 0.5, 0.5, 0.7))],
    );
}

fn apply(buffer: &gtk::TextBuffer, tag: &str, from: i32, to: i32) {
    if to > from {
        buffer.apply_tag_by_name(tag, &buffer.iter_at_offset(from), &buffer.iter_at_offset(to));
    }
}

/// Finds `delim` in `chars` starting at index `from`.
fn find_closing(chars: &[char], from: usize, delim: &[char]) -> Option<usize> {
    let d = delim.len();
    let mut j = from;
    while j + d <= chars.len() {
        if chars[j..j + d] == *delim {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Styles **bold**, *italic* and `code` inside one line.
/// `offset` is the buffer position of the line start; `start` is where to begin scanning.
fn style_inline(buffer: &gtk::TextBuffer, chars: &[char], offset: i32, start: usize) {
    // #tags
    for (a, b) in find_tags(chars, start) {
        apply(buffer, "tag", offset + a as i32, offset + b as i32);
    }

    let n = chars.len();
    let mut i = start;
    while i < n {
        let (delim, tag): (&[char], &str) = if chars[i] == '`' {
            (&['`'][..], "code")
        } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '*' {
            (&['*', '*'][..], "bold")
        } else if chars[i] == '*' {
            (&['*'][..], "italic")
        } else {
            i += 1;
            continue;
        };

        let d = delim.len();
        // closing delimiter must leave at least one character of content
        if let Some(j) = find_closing(chars, i + d + 1, delim) {
            let (a, b) = (i as i32, j as i32);
            let d = d as i32;
            apply(buffer, tag, offset + a, offset + b + d);
            apply(buffer, "marker", offset + a, offset + a + d);
            apply(buffer, "marker", offset + b, offset + b + d);
            i = (b + d) as usize;
        } else {
            i += d;
        }
    }
}

fn style_line(buffer: &gtk::TextBuffer, line: &str, offset: i32, in_code_block: &mut bool) {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len() as i32;

    // ``` fenced code blocks
    if line.trim_start().starts_with("```") {
        *in_code_block = !*in_code_block;
        apply(buffer, "marker", offset, offset + len);
        return;
    }
    if *in_code_block {
        apply(buffer, "code", offset, offset + len);
        return;
    }

    // Headings: "# ", "## ", "### "
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    if (1..=3).contains(&hashes) && chars.get(hashes) == Some(&' ') {
        apply(buffer, &format!("h{hashes}"), offset, offset + len);
        apply(buffer, "marker", offset, offset + hashes as i32 + 1);
        return;
    }

    // Quote: "> text"
    if line.starts_with("> ") {
        apply(buffer, "quote", offset, offset + len);
        apply(buffer, "marker", offset, offset + 2);
        style_inline(buffer, &chars, offset, 2);
        return;
    }

    // Bullet list: "- item" or "* item" (optionally indented)
    let indent = chars.iter().take_while(|c| **c == ' ').count();
    if (chars.get(indent) == Some(&'-') || chars.get(indent) == Some(&'*'))
        && chars.get(indent + 1) == Some(&' ')
    {
        apply(buffer, "bullet", offset + indent as i32, offset + indent as i32 + 1);
        style_inline(buffer, &chars, offset, indent + 2);
        return;
    }

    style_inline(buffer, &chars, offset, 0);
}

/// Re-applies Markdown styling to the whole note.
fn highlight(buffer: &gtk::TextBuffer) {
    let (start, end) = buffer.bounds();
    buffer.remove_all_tags(&start, &end);
    let text = buffer.text(&start, &end, false).to_string();

    let mut offset: i32 = 0; // character offset of the current line
    let mut in_code_block = false;
    for line in text.split('\n') {
        style_line(buffer, line, offset, &mut in_code_block);
        offset += line.chars().count() as i32 + 1;
    }
}

// ───────────────────────── UI helpers ─────────────────────────

/// One sidebar row: title, subtitle, a pin button and an inline trash button.
fn make_row(title: &str, subtitle: &str, pinned: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .title_lines(1)
        .subtitle_lines(1)
        .use_markup(false) // note text may contain & or <
        .build();

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Delete note")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();

    // The row's position changes over time, so look it up when clicked
    delete_btn.connect_clicked(glib::clone!(
        #[weak]
        row,
        move |btn| {
            let _ = btn.activate_action("win.delete-note-at", Some(&row.index().to_variant()));
        }
    ));

    let pin_btn = gtk::Button::builder()
        .label(if pinned { "★" } else { "☆" })
        .tooltip_text(if pinned { "Unpin from top" } else { "Pin to top" })
        .valign(gtk::Align::Center)
        .css_classes(if pinned {
            ["circular", "suggested-action"]
        } else {
            ["flat", "circular"]
        })
        .build();
    pin_btn.connect_clicked(glib::clone!(
        #[weak]
        row,
        move |btn| {
            let _ = btn.activate_action("win.toggle-pin", Some(&row.index().to_variant()));
        }
    ));

    row.add_suffix(&pin_btn);
    row.add_suffix(&delete_btn);
    row
}

fn row_for(note: &Note) -> adw::ActionRow {
    make_row(&note.title(), &note.subtitle(), note.pinned)
}

/// Throws away all sidebar rows and rebuilds them from the notes in `state`.
fn rebuild_list(state: &Rc<RefCell<State>>, list: &gtk::ListBox) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    let rows: Vec<adw::ActionRow> = state.borrow().notes.iter().map(row_for).collect();
    for row in &rows {
        list.append(row);
    }
    let current = state.borrow().current;
    if let Some(i) = current {
        // `current` already matches, so row_selected won't reload the editor
        list.select_row(list.row_at_index(i as i32).as_ref());
    }
}

/// Pins or unpins the note at `idx` and moves it to the right place.
fn toggle_pin(state: &Rc<RefCell<State>>, list: &gtk::ListBox, idx: usize) {
    {
        let mut s = state.borrow_mut();
        if idx >= s.notes.len() {
            return;
        }
        let current_id = s.current.and_then(|c| s.notes.get(c)).map(|n| n.id.clone());

        let mut note = s.notes.remove(idx);
        note.pinned = !note.pinned;
        let at = if note.pinned {
            0 // newly pinned notes go to the very top
        } else {
            s.notes.iter().filter(|n| n.pinned).count() // top of the unpinned notes
        };
        s.notes.insert(at, note);

        let new_current = current_id.and_then(|id| s.notes.iter().position(|n| n.id == id));
        s.current = new_current;
        save_pins(&s.notes);
    }
    rebuild_list(state, list);
}

/// Where an edited note belongs: the top, or just below the pinned notes.
fn top_slot(s: &State, idx: usize) -> usize {
    if s.notes[idx].pinned {
        0
    } else {
        s.notes.iter().filter(|n| n.pinned).count()
    }
}

/// Moves a note (state + sidebar row) from position `from` to position `to`.
fn move_note(state: &Rc<RefCell<State>>, list: &gtk::ListBox, from: usize, to: usize) {
    let Some(row) = list.row_at_index(from as i32) else { return };
    {
        let mut s = state.borrow_mut();
        if from >= s.notes.len() {
            return;
        }
        let note = s.notes.remove(from);
        s.notes.insert(to, note);
        s.current = Some(to);
    }
    list.remove(&row);
    list.insert(&row, to as i32);
    list.select_row(Some(&row)); // row_selected sees current == to and does nothing
}

/// Deletes the note at `idx` (state, file and sidebar row).
fn delete_at(
    state: &Rc<RefCell<State>>,
    list: &gtk::ListBox,
    buffer: &gtk::TextBuffer,
    tags: &TagUi,
    idx: usize,
) {
    let was_current;
    {
        let mut s = state.borrow_mut();
        if idx >= s.notes.len() {
            return;
        }
        let note = s.notes.remove(idx);
        let _ = fs::remove_file(note.path());
        if note.pinned {
            save_pins(&s.notes);
        }

        was_current = s.current == Some(idx);
        if was_current {
            s.current = None;
            s.loading = true; // clearing the editor is not a user edit
        } else if let Some(c) = s.current {
            if idx < c {
                s.current = Some(c - 1); // rows above the open note shifted up
            }
        }
    }
    if was_current {
        buffer.begin_irreversible_action();
        buffer.set_text("");
        buffer.end_irreversible_action();
        state.borrow_mut().loading = false;
    }

    if let Some(row) = list.row_at_index(idx as i32) {
        list.remove(&row);
    }

    let len = state.borrow().notes.len();
    if len == 0 {
        // Never leave the app with no note: start a fresh one
        let note = Note::new();
        let row = row_for(&note);
        state.borrow_mut().notes.insert(0, note);
        list.insert(&row, 0);
        list.select_row(Some(&row));
    } else if was_current {
        let next = idx.min(len - 1) as i32;
        list.select_row(list.row_at_index(next).as_ref());
    }

    refresh_tags(state, tags); // the deleted note may have been the last user of a tag
}

/// Deletes right away if the note is empty, otherwise asks first.
fn request_delete(
    window: &adw::ApplicationWindow,
    state: &Rc<RefCell<State>>,
    list: &gtk::ListBox,
    buffer: &gtk::TextBuffer,
    tags: &Rc<TagUi>,
    idx: usize,
) {
    let info = {
        let s = state.borrow();
        s.notes.get(idx).map(|n| (n.content.trim().is_empty(), n.title()))
    };
    let Some((is_empty, title)) = info else { return };

    if is_empty {
        delete_at(state, list, buffer, tags, idx);
        return;
    }

    let dialog = adw::AlertDialog::new(
        Some("Delete this note?"),
        Some(&format!("“{title}” will be permanently deleted.")),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let (state, list, buffer, tags) = (state.clone(), list.clone(), buffer.clone(), tags.clone());
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            delete_at(&state, &list, &buffer, &tags, idx);
        }
    });
    dialog.present(Some(window));
}

/// Ctrl+B / Ctrl+I: wrap the selection in `marker` (or unwrap it if already wrapped).
fn toggle_wrap(buffer: &gtk::TextBuffer, marker: &str) {
    let m = marker.chars().count() as i32;
    buffer.begin_user_action();

    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let s_off = start.offset();
        let sel = buffer.text(&start, &end, false).to_string();
        let sel_len = sel.chars().count() as i32;
        buffer.delete(&mut start, &mut end);
        let mut at = buffer.iter_at_offset(s_off);

        let already_wrapped = sel.len() > marker.len() * 2
            && sel.starts_with(marker)
            && sel.ends_with(marker)
            && !(marker == "*" && sel.starts_with("**"));

        let (new_from, new_to) = if already_wrapped {
            buffer.insert(&mut at, &sel[marker.len()..sel.len() - marker.len()]);
            (s_off, s_off + sel_len - 2 * m)
        } else {
            buffer.insert(&mut at, &format!("{marker}{sel}{marker}"));
            (s_off + m, s_off + m + sel_len)
        };
        buffer.select_range(&buffer.iter_at_offset(new_from), &buffer.iter_at_offset(new_to));
    } else {
        // Nothing selected: insert an empty pair and put the cursor inside
        let mut at = buffer.iter_at_offset(buffer.cursor_position());
        let off = at.offset();
        buffer.insert(&mut at, &format!("{marker}{marker}"));
        buffer.place_cursor(&buffer.iter_at_offset(off + m));
    }

    buffer.end_user_action();
}

/// "12 words · 68 characters"
fn stats_text(text: &str) -> String {
    let words = text.split_whitespace().count();
    let chars = text.chars().count();
    format!(
        "{words} {} · {chars} {}",
        if words == 1 { "word" } else { "words" },
        if chars == 1 { "character" } else { "characters" },
    )
}

fn show_shortcuts(window: &adw::ApplicationWindow) {
    let groups: [(&str, &[(&str, &str)]); 3] = [
        (
            "Notes",
            &[
                ("New note", "Ctrl+N"),
                ("Delete note", "Ctrl+D"),
                ("Pin / unpin note", "Ctrl+P"),
                ("Search notes", "Ctrl+F"),
            ],
        ),
        (
            "Editing",
            &[
                ("Bold", "Ctrl+B"),
                ("Italic", "Ctrl+I"),
                ("Undo", "Ctrl+Z"),
                ("Redo", "Ctrl+Shift+Z"),
            ],
        ),
        (
            "General",
            &[
                ("Keyboard shortcuts", "Ctrl+? or F1"),
                ("Toggle full screen", "F11"),
                ("Quit", "Ctrl+Q"),
            ],
        ),
    ];

    let page = adw::PreferencesPage::new();
    for (title, items) in groups {
        let group = adw::PreferencesGroup::builder().title(title).build();
        for (name, keys) in items {
            let row = adw::ActionRow::builder().title(*name).build();
            row.add_suffix(
                &gtk::Label::builder()
                    .label(*keys)
                    .valign(gtk::Align::Center)
                    .css_classes(["dim-label"])
                    .build(),
            );
            group.add(&row);
        }
        page.add(&group);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(440)
        .content_height(580)
        .child(&toolbar)
        .build();
    dialog.present(Some(window));
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    apply_theme(&load_theme());

    // ----- Load notes (create a welcome note on first run) -----
    let mut notes = load_notes();
    if notes.is_empty() {
        let mut welcome = Note::new();
        welcome.content = "# Welcome to Notes\n\n\
            Start typing here. Your notes are saved automatically.\n\n\
            ## Markdown works\n\n\
            - **Bold**, *italic* and `code`\n\
            - Headings with `#`, `##`, `###`\n\
            - Lists with `-`\n\n\
            > Quotes start with `>`\n\n\
            #welcome #tips\n"
            .to_string();
        welcome.dirty = true;
        notes.push(welcome);
    }

    let state = Rc::new(RefCell::new(State {
        notes,
        current: None,
        loading: false,
        save_pending: false,
    }));

    // Current search text (stored lowercase)
    let query = Rc::new(RefCell::new(String::new()));

    // The tag chip currently selected (None = show all notes)
    let tag_filter: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // ----- Sidebar -----
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();

    let rows: Vec<adw::ActionRow> = state.borrow().notes.iter().map(row_for).collect();
    for row in &rows {
        list.append(row);
    }

    // Shown when the search matches nothing
    let placeholder = gtk::Label::builder()
        .label("No matching notes")
        .css_classes(["dim-label"])
        .margin_top(24)
        .margin_bottom(24)
        .build();
    list.set_placeholder(Some(&placeholder));

    // Filter rows by the search text (title + body) and the selected tag chip
    list.set_filter_func(glib::clone!(
        #[strong]
        state,
        #[strong]
        query,
        #[strong]
        tag_filter,
        move |row| {
            let s = state.borrow();
            let Some(note) = s.notes.get(row.index() as usize) else {
                return true;
            };
            if let Some(tag) = tag_filter.borrow().as_ref() {
                if !extract_tags(&note.content).contains(tag) {
                    return false;
                }
            }
            let q = query.borrow();
            q.is_empty() || note.content.to_lowercase().contains(q.as_str())
        }
    ));

    let new_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("New note (Ctrl+N)")
        .action_name("win.new-note")
        .build();

    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.pack_start(&new_btn);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search notes")
        .hexpand(true)
        .build();
    let search_box = gtk::Box::builder()
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(8)
        .build();
    search_box.append(&search);

    // Tag chips (hidden until at least one #tag exists)
    let tag_bar = gtk::Box::builder()
        .spacing(6)
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(8)
        .build();
    let tag_scroll = gtk::ScrolledWindow::builder()
        .child(&tag_bar)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .visible(false)
        .build();
    let tags = Rc::new(TagUi {
        scroller: tag_scroll.clone(),
        bar: tag_bar,
        list: list.clone(),
        filter: tag_filter.clone(),
        shown: RefCell::new(Vec::new()),
    });
    refresh_tags(&state, &tags);

    let sidebar_toolbar = adw::ToolbarView::new();
    sidebar_toolbar.add_top_bar(&sidebar_header);
    sidebar_toolbar.add_top_bar(&search_box);
    sidebar_toolbar.add_top_bar(&tag_scroll);
    sidebar_toolbar.set_content(Some(&gtk::ScrolledWindow::builder().child(&list).build()));

    let sidebar_page = adw::NavigationPage::builder()
        .title("Notes")
        .child(&sidebar_toolbar)
        .build();

    // ----- Editor -----
    let text = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(18)
        .bottom_margin(18)
        .left_margin(24)
        .right_margin(24)
        .pixels_below_lines(4)
        .build();
    let buffer = text.buffer();
    create_tags(&buffer);

    // Main menu: theme + help
    let menu = gio::Menu::new();
    let theme_section = gio::Menu::new();
    theme_section.append(Some("System theme"), Some("win.theme::system"));
    theme_section.append(Some("Light"), Some("win.theme::light"));
    theme_section.append(Some("Dark"), Some("win.theme::dark"));
    menu.append_section(Some("Appearance"), &theme_section);
    let help_section = gio::Menu::new();
    help_section.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    help_section.append(Some("About Notes"), Some("win.about"));
    menu.append_section(None, &help_section);

    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .menu_model(&menu)
        .build();

    let content_header = adw::HeaderBar::new();
    content_header.pack_end(&menu_button);

    // Word / character count under the editor
    let stats = gtk::Label::builder()
        .halign(gtk::Align::End)
        .margin_end(18)
        .margin_top(6)
        .margin_bottom(6)
        .css_classes(["dim-label", "caption"])
        .build();

    let content_toolbar = adw::ToolbarView::new();
    content_toolbar.add_top_bar(&content_header);
    content_toolbar.add_bottom_bar(&stats);
    content_toolbar.set_content(Some(&gtk::ScrolledWindow::builder().child(&text).build()));

    let content_page = adw::NavigationPage::builder()
        .title("Note")
        .child(&content_toolbar)
        .build();

    // ----- Behaviour: click a note in the sidebar -> show it in the editor -----
    list.connect_row_selected(glib::clone!(
        #[strong]
        state,
        #[weak]
        buffer,
        move |_, row| {
            let Some(row) = row else { return };
            let idx = row.index() as usize;

            let content = {
                let mut s = state.borrow_mut();
                if s.current == Some(idx) {
                    return; // already showing this note (e.g. after moving it to the top)
                }
                let Some(note) = s.notes.get(idx) else { return };
                let content = note.content.clone();
                s.current = Some(idx);
                s.loading = true; // don't treat this set_text as a user edit
                content
            };
            // Loading a note is not an edit: keep it out of the undo history,
            // otherwise Ctrl+Z could bring back the previous note's text
            buffer.begin_irreversible_action();
            buffer.set_text(&content);
            buffer.end_irreversible_action();
            state.borrow_mut().loading = false;
        }
    ));

    // ----- Behaviour: user edits the text -> style, update state, reorder, save -----
    buffer.connect_changed(glib::clone!(
        #[strong]
        state,
        #[strong]
        tags,
        #[weak]
        list,
        #[weak]
        stats,
        move |buf| {
            highlight(buf); // style Markdown (also when a note is loaded)
            stats.set_text(&stats_text(&buf.text(&buf.start_iter(), &buf.end_iter(), false)));

            let (mut idx, target, schedule_save) = {
                let mut s = state.borrow_mut();
                if s.loading {
                    return;
                }
                let Some(idx) = s.current else { return };
                if idx >= s.notes.len() {
                    return;
                }

                let new_text = buf
                    .text(&buf.start_iter(), &buf.end_iter(), false)
                    .to_string();
                s.notes[idx].content = new_text;
                s.notes[idx].dirty = true;
                s.notes[idx].modified = SystemTime::now();

                let schedule_save = !s.save_pending;
                s.save_pending = true;
                let target = top_slot(&s, idx);
                (idx, target, schedule_save)
            };

            // Most recently edited note goes to the top (below the pinned notes)
            if idx != target {
                move_note(&state, &list, idx, target);
                idx = target;
            }

            // Keep the sidebar title/preview in sync with the note
            let (title, subtitle) = {
                let s = state.borrow();
                (s.notes[idx].title(), s.notes[idx].subtitle())
            };
            if let Some(row) = list.row_at_index(idx as i32).and_downcast::<adw::ActionRow>() {
                row.set_title(&title);
                row.set_subtitle(&subtitle);
            }

            // Save shortly after typing stops (one timer at a time)
            if schedule_save {
                glib::timeout_add_local_once(
                    SAVE_DELAY,
                    glib::clone!(
                        #[strong]
                        state,
                        #[strong]
                        tags,
                        move || {
                            {
                                let mut s = state.borrow_mut();
                                s.save_dirty();
                                s.save_pending = false;
                            }
                            refresh_tags(&state, &tags); // pick up new or removed #tags
                        }
                    ),
                );
            }
        }
    ));

    // ----- Behaviour: search -----
    search.connect_search_changed(glib::clone!(
        #[strong]
        query,
        #[weak]
        list,
        move |entry| {
            *query.borrow_mut() = entry.text().to_lowercase();
            list.invalidate_filter();
        }
    ));
    search.connect_stop_search(|entry| entry.set_text("")); // Esc clears

    // Select the first note now that all handlers are connected
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }

    // ----- Window -----
    let split = adw::NavigationSplitView::new();
    split.set_min_sidebar_width(300.0);
    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Notes")
        .icon_name(APP_ID)
        .default_width(1000)
        .default_height(650)
        .content(&split)
        .build();

    // ----- Actions (used by buttons and keyboard shortcuts) -----

    // New note
    let new_action = gio::SimpleAction::new("new-note", None);
    new_action.connect_activate(glib::clone!(
        #[strong]
        state,
        #[weak]
        list,
        #[weak]
        text,
        #[weak]
        search,
        move |_, _| {
            search.set_text(""); // make sure the new note will be visible

            // If the current note is still empty, reuse it instead of piling up blanks
            let reuse = {
                let s = state.borrow();
                s.current
                    .and_then(|i| s.notes.get(i))
                    .map_or(false, |n| n.content.is_empty())
            };
            if reuse {
                text.grab_focus();
                return;
            }

            let note = Note::new();
            let row = row_for(&note);
            let at = {
                let mut s = state.borrow_mut();
                let at = s.notes.iter().filter(|n| n.pinned).count(); // below pinned notes
                s.notes.insert(at, note);
                s.current = None; // indexes shifted; let row_selected load the new note
                at
            };
            list.insert(&row, at as i32);
            list.select_row(Some(&row));
            text.grab_focus();
        }
    ));
    window.add_action(&new_action);

    // Delete the open note (Ctrl+D)
    let delete_action = gio::SimpleAction::new("delete-note", None);
    delete_action.connect_activate(glib::clone!(
        #[strong]
        state,
        #[strong]
        tags,
        #[weak]
        list,
        #[weak]
        buffer,
        #[weak]
        window,
        move |_, _| {
            let current = state.borrow().current;
            if let Some(idx) = current {
                request_delete(&window, &state, &list, &buffer, &tags, idx);
            }
        }
    ));
    window.add_action(&delete_action);

    // Delete a specific note (the trash button inside each sidebar row)
    let delete_at_action = gio::SimpleAction::new("delete-note-at", Some(glib::VariantTy::INT32));
    delete_at_action.connect_activate(glib::clone!(
        #[strong]
        state,
        #[strong]
        tags,
        #[weak]
        list,
        #[weak]
        buffer,
        #[weak]
        window,
        move |_, param| {
            let idx = param.and_then(|p| p.get::<i32>()).unwrap_or(-1);
            if idx >= 0 {
                request_delete(&window, &state, &list, &buffer, &tags, idx as usize);
            }
        }
    ));
    window.add_action(&delete_at_action);

    // Pin / unpin a specific note (the pin button inside each sidebar row)
    let pin_action = gio::SimpleAction::new("toggle-pin", Some(glib::VariantTy::INT32));
    pin_action.connect_activate(glib::clone!(
        #[strong]
        state,
        #[weak]
        list,
        move |_, param| {
            let idx = param.and_then(|p| p.get::<i32>()).unwrap_or(-1);
            if idx >= 0 {
                // Wait until the button's click has finished before rebuilding its row
                glib::idle_add_local_once(glib::clone!(
                    #[strong]
                    state,
                    #[weak]
                    list,
                    move || toggle_pin(&state, &list, idx as usize)
                ));
            }
        }
    ));
    window.add_action(&pin_action);

    // Pin / unpin the open note (Ctrl+P)
    let pin_current_action = gio::SimpleAction::new("pin-note", None);
    pin_current_action.connect_activate(glib::clone!(
        #[strong]
        state,
        #[weak]
        list,
        move |_, _| {
            let current = state.borrow().current;
            if let Some(idx) = current {
                toggle_pin(&state, &list, idx);
            }
        }
    ));
    window.add_action(&pin_current_action);

    // Bold / italic
    let bold_action = gio::SimpleAction::new("bold", None);
    bold_action.connect_activate(glib::clone!(
        #[weak]
        buffer,
        move |_, _| toggle_wrap(&buffer, "**")
    ));
    window.add_action(&bold_action);

    let italic_action = gio::SimpleAction::new("italic", None);
    italic_action.connect_activate(glib::clone!(
        #[weak]
        buffer,
        move |_, _| toggle_wrap(&buffer, "*")
    ));
    window.add_action(&italic_action);

    // Theme (System / Light / Dark), remembered between runs
    let theme_action = gio::SimpleAction::new_stateful(
        "theme",
        Some(glib::VariantTy::STRING),
        &load_theme().to_variant(),
    );
    theme_action.connect_activate(|action, param| {
        if let Some(theme) = param.and_then(|p| p.get::<String>()) {
            apply_theme(&theme);
            save_theme(&theme);
            action.set_state(&theme.to_variant());
        }
    });
    window.add_action(&theme_action);

    // Keyboard shortcuts window
    let shortcuts_action = gio::SimpleAction::new("shortcuts", None);
    shortcuts_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| show_shortcuts(&window)
    ));
    window.add_action(&shortcuts_action);

    // About dialog
    let about_action = gio::SimpleAction::new("about", None);
    about_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| {
            let about = adw::AboutDialog::builder()
                .application_name("Notes")
                .application_icon(APP_ID)
                .developer_name("Your Name") // <- put your name here
                .version(env!("CARGO_PKG_VERSION"))
                .comments("A modern, minimal note-taking app built with Rust, GTK4 and libadwaita.")
                .copyright("© 2026 Your Name") // <- and here
                .license_type(gtk::License::MitX11)
                .build();
            about.present(Some(&window));
        }
    ));
    window.add_action(&about_action);

    // Full screen (F11) and quit (Ctrl+Q)
    let fullscreen_action = gio::SimpleAction::new("fullscreen", None);
    fullscreen_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| window.set_fullscreened(!window.is_fullscreen())
    ));
    window.add_action(&fullscreen_action);

    let quit_action = gio::SimpleAction::new("quit", None);
    quit_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| window.close() // goes through close-request, so notes get saved
    ));
    window.add_action(&quit_action);

    // Focus search
    let search_action = gio::SimpleAction::new("search", None);
    search_action.connect_activate(glib::clone!(
        #[weak]
        search,
        move |_, _| {
            search.grab_focus();
        }
    ));
    window.add_action(&search_action);

    // Keyboard shortcuts
    app.set_accels_for_action("win.new-note", &["<Control>n"]);
    app.set_accels_for_action("win.delete-note", &["<Control>d"]);
    app.set_accels_for_action("win.search", &["<Control>f"]);
    app.set_accels_for_action("win.pin-note", &["<Control>p"]);
    app.set_accels_for_action("win.bold", &["<Control>b"]);
    app.set_accels_for_action("win.italic", &["<Control>i"]);
    app.set_accels_for_action("win.shortcuts", &["<Control>question", "F1"]);
    app.set_accels_for_action("win.fullscreen", &["F11"]);
    app.set_accels_for_action("win.quit", &["<Control>q"]);

    // Make sure nothing is lost when the window closes
    window.connect_close_request(glib::clone!(
        #[strong]
        state,
        move |_| {
            state.borrow_mut().save_dirty();
            glib::Propagation::Proceed
        }
    ));

    window.present();
}

// ───────────────────────── Tests ─────────────────────────
// Run with:  cargo test
// These cover the pure logic (tags, titles, previews, counters). They never
// touch your real notes.

#[cfg(test)]
mod tests {
    use super::*;

    fn note(content: &str) -> Note {
        let mut n = Note::new();
        n.content = content.to_string();
        n
    }

    fn tags_in(line: &str) -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        find_tags(&chars, 0)
            .into_iter()
            .map(|(a, b)| chars[a..b].iter().collect())
            .collect()
    }

    // ----- tags -----

    #[test]
    fn finds_simple_tags() {
        assert_eq!(tags_in("buy milk #personal"), vec!["#personal"]);
        assert_eq!(tags_in("#work and #urgent"), vec!["#work", "#urgent"]);
    }

    #[test]
    fn tag_stops_at_punctuation_but_keeps_dashes_and_underscores() {
        assert_eq!(tags_in("#work-item, #to_do."), vec!["#work-item", "#to_do"]);
    }

    #[test]
    fn headings_and_odd_hashes_are_not_tags() {
        assert!(tags_in("# Heading").is_empty());
        assert!(tags_in("## Heading").is_empty());
        assert!(tags_in("issue #123").is_empty()); // must start with a letter
        assert!(tags_in("a#b").is_empty()); // must start a word
        assert!(tags_in("just a #").is_empty());
    }

    #[test]
    fn works_with_non_ascii_text() {
        assert_eq!(tags_in("café #über"), vec!["#über"]);
    }

    #[test]
    fn tag_only_lines_are_detected() {
        assert!(is_tag_line("#work #ideas"));
        assert!(is_tag_line("  #solo  "));
        assert!(!is_tag_line("#work and more"));
        assert!(!is_tag_line("plain text"));
        assert!(!is_tag_line(""));
    }

    #[test]
    fn extract_tags_dedupes_lowercases_sorts_and_skips_code() {
        let text = "# Title\n#Work #work #alpha\n```\n#insidecode\n```\nafter #zeta";
        assert_eq!(extract_tags(text), vec!["#alpha", "#work", "#zeta"]);
    }

    // ----- titles and previews -----

    #[test]
    fn title_uses_first_line_without_hashes() {
        assert_eq!(note("# Hello world\nbody").title(), "Hello world");
        assert_eq!(note("### Deep\nbody").title(), "Deep");
        assert_eq!(note("plain first line").title(), "plain first line");
    }

    #[test]
    fn title_skips_blank_lines_and_tag_lines() {
        assert_eq!(note("\n\n# Real title").title(), "Real title");
        assert_eq!(note("#work #urgent\n# Real title").title(), "Real title");
    }

    #[test]
    fn title_of_empty_note() {
        assert_eq!(note("").title(), "Untitled note");
        assert_eq!(note("   \n  ").title(), "Untitled note");
        assert_eq!(note("#").title(), "Untitled note");
    }

    #[test]
    fn subtitle_shows_preview_of_second_line() {
        let s = note("# Title\n- first item\nmore").subtitle();
        assert!(s.starts_with("Just now"), "got: {s}");
        assert!(s.ends_with("first item"), "got: {s}");
    }

    #[test]
    fn subtitle_ignores_tag_lines_and_handles_short_notes() {
        assert_eq!(note("").subtitle(), "Empty note");
        assert_eq!(note("only a title").subtitle(), "Just now");
        assert!(note("Title\n#work\nreal preview").subtitle().ends_with("real preview"));
    }

    #[test]
    fn format_time_for_recent_edit() {
        assert_eq!(format_time(SystemTime::now()), "Just now");
    }

    // ----- counters -----

    #[test]
    fn stats_text_counts_words_and_characters() {
        assert_eq!(stats_text(""), "0 words · 0 characters");
        assert_eq!(stats_text("a"), "1 word · 1 character");
        assert_eq!(stats_text("hello"), "1 word · 5 characters");
        assert_eq!(stats_text("two  words\nand\tmore"), "4 words · 19 characters");
        assert_eq!(stats_text("héllo wörld"), "2 words · 11 characters"); // counts characters, not bytes
    }

    // ----- state helpers -----

    #[test]
    fn top_slot_goes_below_pinned_notes() {
        let mut pinned_a = note("a");
        pinned_a.pinned = true;
        let mut pinned_b = note("b");
        pinned_b.pinned = true;
        let state = State {
            notes: vec![pinned_a, pinned_b, note("c"), note("d")],
            current: None,
            loading: false,
            save_pending: false,
        };
        assert_eq!(top_slot(&state, 0), 0); // pinned stays at the very top
        assert_eq!(top_slot(&state, 3), 2); // unpinned goes right below the 2 pinned
    }

    #[test]
    fn all_tags_merges_every_note() {
        let state = State {
            notes: vec![note("#b one"), note("#a two #b"), note("none")],
            current: None,
            loading: false,
            save_pending: false,
        };
        assert_eq!(all_tags(&state), vec!["#a", "#b"]);
    }

    #[test]
    fn find_closing_needs_the_delimiter() {
        let chars: Vec<char> = "ab**cd".chars().collect();
        assert_eq!(find_closing(&chars, 0, &['*', '*']), Some(2));
        assert_eq!(find_closing(&chars, 3, &['*', '*']), None);
    }

    // ----- saving -----

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("notes-app-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("123.md");

        write_atomic(&file, "first version").unwrap();
        write_atomic(&file, "second version").unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "second version");
        assert!(!dir.join("123.tmp").exists(), "temp file was left behind");
        let _ = fs::remove_dir_all(&dir);
    }
}
