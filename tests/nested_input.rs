//! Phase 1 check: with CUL_NESTED=1 every input tool reaches a headless
//! sway, and nothing reaches the host.
//!
//! The test drives the real MCP server over stdio. Clicks, scrolls and drags
//! land in a foot terminal that reports mouse events to a file. Typing goes
//! to a zenity entry and to a foot running `cat`.

mod common;

use common::{have, host_cursor, missing, read_file, wait_for, HeadlessSway};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/nested/mouse_report.py"
);

fn mouse_report(sway: &HeadlessSway, name: &str, mode: &str) -> std::path::PathBuf {
    let out = sway.dir.join(format!("{name}.txt"));
    sway.exec(&format!(
        "foot -a {name} -o pad=0x0 python3 {FIXTURE} {} {mode}",
        out.display()
    ));
    assert!(sway.wait_focused(name), "{name} never took focus");
    // Give the fixture time to switch the terminal into mouse mode.
    std::thread::sleep(Duration::from_millis(400));
    out
}

/// Parse a report of button, column and row joined by semicolons, plus its M or m suffix.
fn parse_report(report: &str) -> (u32, u32, u32) {
    let fields = report
        .trim_end_matches(['M', 'm'])
        .split(';')
        .map(|field| field.parse::<u32>().expect("numeric report field"))
        .collect::<Vec<_>>();
    (fields[0], fields[1], fields[2])
}

fn wait_report(path: &Path) -> String {
    wait_for(Duration::from_secs(5), || read_file(path))
        .unwrap_or_else(|| panic!("no mouse report in {}", path.display()))
}

#[test]
fn every_input_tool_reaches_the_nested_display_only() {
    if let Some(program) = missing(&["sway", "swaymsg", "foot", "python3", "grim"]) {
        eprintln!("skipping: {program} is not installed");
        return;
    }
    let sway = HeadlessSway::start(1280, 800);
    let mut mcp = sway.mcp();
    let cursor_before = host_cursor();

    // A click at the top-left corner lands in the first cell.
    let corner = mouse_report(&sway, "clickcorner", "press");
    let result = mcp.call("click", json!({"x": 3, "y": 3}));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(wait_report(&corner), "0;1;1M");

    // A click at the centre lands in the middle cell.
    let centre = mouse_report(&sway, "clickcentre", "press");
    let result = mcp.call("click", json!({"x": 640, "y": 400}));
    assert_eq!(result["ok"], true, "{result}");
    let (button, column, row) = parse_report(&wait_report(&centre));
    assert_eq!(button, 0);
    let screen = mcp.call("screenshot", json!({"max_width": 320}));
    let screen: serde_json::Value = screen["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find_map(|block| block["text"].as_str()))
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(screen);
    assert_eq!(screen["source"], "grim", "{screen}");
    assert_eq!(screen["coordinate_width"], 1280, "{screen}");
    // Foot fills the output, so the centre cell is half the grid. Allow a
    // cell of rounding either way.
    let columns_estimate = column * 2;
    let rows_estimate = row * 2;
    assert!(
        columns_estimate > 100 && rows_estimate > 30,
        "{column};{row}"
    );

    // A right click reports button 2.
    let right = mouse_report(&sway, "clickright", "press");
    let result = mcp.call("click", json!({"x": 200, "y": 200, "button": "right"}));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(parse_report(&wait_report(&right)).0, 2);

    // A scroll down reports wheel button 65.
    let wheel = mouse_report(&sway, "wheel", "press");
    let result = mcp.call("scroll", json!({"direction": "down", "x": 300, "y": 300}));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(parse_report(&wait_report(&wheel)).0, 65);

    // A drag presses at the start, moves with the button held, and releases
    // at the end.
    let drag = mouse_report(&sway, "drag", "drag");
    let result = mcp.call(
        "drag",
        json!({"start_x": 100, "start_y": 100, "end_x": 700, "end_y": 500}),
    );
    assert_eq!(result["ok"], true, "{result}");
    let reports = wait_report(&drag);
    let reports = reports.split(' ').collect::<Vec<_>>();
    let (_, start_column, start_row) = parse_report(reports[0]);
    let (_, end_column, end_row) = parse_report(reports.last().expect("a release"));
    assert!(reports[0].ends_with('M'), "{reports:?}");
    assert!(reports.last().unwrap().ends_with('m'), "{reports:?}");
    assert!(
        reports.iter().any(|report| report.starts_with("32;")),
        "{reports:?}"
    );
    assert!(
        end_column > start_column && end_row > start_row,
        "{reports:?}"
    );

    // Typing reaches a terminal, any character with a keysym included, and
    // a chord ends the input.
    let typed = sway.dir.join("typed.txt");
    sway.exec(&format!("foot -a typing sh -c 'cat > {}'", typed.display()));
    assert!(sway.wait_focused("typing"));
    let result = mcp.call("type_text", json!({"text": "hello Wörld ☺ $#@!\n"}));
    assert_eq!(result["ok"], true, "{result}");
    let result = mcp.call("press_key", json!({"key": "Ctrl+d"}));
    assert_eq!(result["ok"], true, "{result}");
    let text = wait_for(Duration::from_secs(5), || {
        read_file(&typed).filter(|text| text.ends_with('\n'))
    });
    assert_eq!(text.as_deref(), Some("hello Wörld ☺ $#@!\n"));

    // Typing and Enter reach a GTK dialog.
    if have("zenity") {
        let answer = sway.dir.join("zenity.txt");
        sway.exec(&format!(
            "zenity --entry --text nested > {}",
            answer.display()
        ));
        assert!(sway.wait_focused("zenity"), "zenity never took focus");
        std::thread::sleep(Duration::from_millis(500));
        let result = mcp.call("type_text", json!({"text": "nested ok"}));
        assert_eq!(result["ok"], true, "{result}");
        let result = mcp.call("press_key", json!({"key": "Enter"}));
        assert_eq!(result["ok"], true, "{result}");
        let text = wait_for(Duration::from_secs(5), || read_file(&answer));
        assert_eq!(text.as_deref().map(str::trim), Some("nested ok"));
    }

    // Window tools see the nested sway and nothing on the host.
    sway.exec("foot -a listed");
    assert!(sway.wait_focused("listed"));
    let windows = mcp.call("list_windows", json!({}));
    assert_eq!(windows["backend"], "i3", "{windows}");
    let listed = windows["windows"].as_array().cloned().unwrap_or_default();
    assert_eq!(listed.len(), 1, "{windows}");
    assert_eq!(listed[0]["app_id"], "listed", "{windows}");
    let result = mcp.call(
        "press_key",
        json!({"key": "Ctrl+d", "window_id": listed[0]["window_id"]}),
    );
    assert_eq!(result["ok"], true, "{result}");

    let cursor_after = host_cursor();
    eprintln!("host cursor before {cursor_before:?} after {cursor_after:?}");
    if std::env::var_os("CUL_ASSERT_HOST_CURSOR").is_some() {
        assert_eq!(cursor_before, cursor_after, "the host cursor moved");
    }
}
