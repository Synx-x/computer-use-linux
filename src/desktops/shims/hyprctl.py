#!/usr/bin/env python3
"""hyprctl for an agent desktop.

The user's Hyprland binds and scripts call hyprctl. Inside a desktop there is no
Hyprland, only the desktop's own sway, so this shim answers instead: it turns
dispatch, batch, reload and monitor keywords into sway commands, and builds
Hyprland-shaped JSON for the queries scripts make (activewindow, activeworkspace,
workspaces, monitors, clients). Read-only queries a desktop cannot answer, such as
binds and getoption, go to the host Hyprland. Nothing here changes the host.

`hyprctl --serve-ipc SIGNATURE` runs a stand-in for Hyprland's IPC sockets at
$XDG_RUNTIME_DIR/hypr/SIGNATURE. The desktop's shell and apps get that signature,
so Hyprland-aware programs, Caelestia among them, see one focused monitor and the
sway workspaces and windows, and their dispatches turn into sway commands.
"""
import json
import os
import re
import subprocess
import sys

HOST_SIGNATURE = "__HOST_SIGNATURE__"
HOST_READ_ONLY = {"binds", "getoption", "version", "devices", "layers", "cursorpos"}
TRANSFORMS = {"0": "normal", "1": "90", "2": "180", "3": "270",
              "4": "flipped", "5": "flipped-90", "6": "flipped-180", "7": "flipped-270"}
DIRECTIONS = {"l": "left", "left": "left", "r": "right", "right": "right",
              "u": "up", "up": "up", "t": "up", "d": "down", "down": "down", "b": "down"}


def sway(command=None, kind=None):
    args = ["swaymsg", "-r"]
    if kind:
        args += ["-t", kind]
    if command:
        args.append(command)
    result = subprocess.run(args, capture_output=True, text=True)
    return result.returncode, result.stdout


def query(kind):
    code, out = sway(kind=kind)
    return json.loads(out) if code == 0 and out.strip() else []


def walk(node, workspace=None, output=None):
    if node.get("type") == "output":
        output = node.get("name")
    if node.get("type") == "workspace":
        workspace = node
    yield node, workspace, output
    for child in node.get("nodes", []) + node.get("floating_nodes", []):
        yield from walk(child, workspace, output)


def views():
    tree = query("get_tree")
    for node, workspace, output in walk(tree) if tree else []:
        if node.get("type") in ("con", "floating_con") and node.get("pid") and not node.get("nodes"):
            yield node, workspace, output


def client(node, workspace, output):
    rect = node.get("rect", {})
    ws = workspace or {}
    return {
        "address": hex(node["id"]),
        "mapped": True,
        "hidden": False,
        "at": [rect.get("x", 0), rect.get("y", 0)],
        "size": [rect.get("width", 0), rect.get("height", 0)],
        "workspace": {"id": ws.get("num", -1) if ws.get("name") != "__i3_scratch" else -99,
                      "name": "special:scratchpad" if ws.get("name") == "__i3_scratch" else ws.get("name", "")},
        "floating": node.get("type") == "floating_con",
        "pseudo": False,
        "monitor": 0,
        "class": node.get("app_id") or "",
        "title": node.get("name") or "",
        "initialClass": node.get("app_id") or "",
        "initialTitle": node.get("name") or "",
        "pid": node.get("pid", -1),
        "xwayland": False,
        "pinned": bool(node.get("sticky")),
        "fullscreen": 2 if node.get("fullscreen_mode") else 0,
        "fullscreenClient": 2 if node.get("fullscreen_mode") else 0,
        "focusHistoryID": 0 if node.get("focused") else 1,
    }


def workspace_json(ws):
    outputs = {o["name"]: i for i, o in enumerate(query("get_outputs"))}
    return {"id": ws.get("num", -1), "name": ws.get("name", ""), "monitor": ws.get("output", ""),
            "monitorID": outputs.get(ws.get("output"), 0), "windows": len(ws.get("nodes", [])) + len(ws.get("floating_nodes", [])),
            "hasfullscreen": False, "lastwindow": "0x0", "lastwindowtitle": ""}


def monitors_json():
    workspaces = query("get_workspaces")
    out = []
    for i, o in enumerate(query("get_outputs")):
        rect, mode = o.get("rect", {}), o.get("current_mode", {})
        ws = next((w for w in workspaces if w.get("output") == o["name"] and w.get("visible")), {})
        out.append({
            "id": i, "name": o["name"], "description": o.get("make", ""), "make": o.get("make", ""),
            "model": o.get("model", ""), "width": mode.get("width", rect.get("width", 0)),
            "height": mode.get("height", rect.get("height", 0)), "refreshRate": (mode.get("refresh") or 60000) / 1000,
            "x": rect.get("x", 0), "y": rect.get("y", 0),
            "activeWorkspace": {"id": ws.get("num", 1), "name": ws.get("name", "1")},
            "specialWorkspace": {"id": 0, "name": ""}, "reserved": [0, 0, 0, 0],
            "scale": o.get("scale", 1.0), "transform": next((int(k) for k, v in TRANSFORMS.items() if v == o.get("transform")), 0),
            "focused": bool(o.get("focused")), "dpmsStatus": bool(o.get("power", True)), "disabled": not o.get("active", True),
        })
    return out


def answer(what, as_json):
    if what == "activewindow":
        focused = next((client(*v) for v in views() if v[0].get("focused")), None)
        data = focused or {}
    elif what == "activeworkspace":
        data = workspace_json(next((w for w in query("get_workspaces") if w.get("focused")), {}))
    elif what == "workspaces":
        data = [workspace_json(w) for w in query("get_workspaces")]
    elif what == "monitors":
        data = monitors_json()
    elif what == "clients":
        data = [client(*v) for v in views()]
    else:
        return None
    if as_json:
        print(json.dumps(data, indent=2))
    elif isinstance(data, list):
        for item in data:
            print("\n".join(f"\t{k}: {v}" for k, v in item.items()) + "\n")
    else:
        print("\n".join(f"\t{k}: {v}" for k, v in data.items()))
    return 0


def criteria(selector):
    """Hyprland window selector (class:^(x)$, title:..., pid:N, address:0x..) as sway criteria."""
    kind, _, value = selector.partition(":")
    if kind in ("class", "initialclass"):
        return f'[app_id="{value}"]'
    if kind in ("title", "initialtitle"):
        return f'[title="{value}"]'
    if kind == "pid":
        return f"[pid={value}]"
    if kind == "address":
        return f"[con_id={int(value, 16)}]"
    return ""


def workspace_target(arg):
    if arg in ("e+1", "r+1", "+1", "m+1"):
        return "workspace next"
    if arg in ("e-1", "r-1", "-1", "m-1"):
        return "workspace prev"
    if arg in ("previous", "previous_per_monitor"):
        return "workspace back_and_forth"
    if arg.isdigit():
        return f"workspace number {arg}"
    if arg.startswith("name:"):
        return f"workspace {arg[5:]}"
    return f"workspace {arg}" if arg else None


def dispatch(name, arg):
    """A sway command for a Hyprland dispatcher. Empty string means accepted with no effect."""
    arg = arg.strip()
    first = arg.split(" ")[0] if arg else ""
    if name == "exec":
        return f"exec {arg}"
    if name in ("killactive", "forcekillactive"):
        return "kill"
    if name in ("togglefloating", "setfloating", "settiled"):
        return {"setfloating": "floating enable", "settiled": "floating disable"}.get(name, "floating toggle")
    if name in ("fullscreen", "fakefullscreen", "fullscreenstate"):
        return "fullscreen toggle"
    if name in ("movefocus", "movewindow", "swapwindow"):
        d = DIRECTIONS.get(first)
        if not d:
            return ""
        return f"focus {d}" if name == "movefocus" else f"move {d}"
    if name == "cyclenext":
        return "focus prev" if "prev" in arg else "focus next"
    if name == "centerwindow":
        return "move position center"
    if name == "pin":
        return "sticky toggle"
    if name == "layoutmsg":
        return "layout toggle split" if "togglesplit" in arg or "swapsplit" in arg else ""
    if name == "togglegroup":
        return "layout toggle tabbed split"
    if name in ("changegroupactive",):
        return "focus next sibling" if first in ("f", "forward") else "focus prev sibling"
    if name == "submap":
        return "mode default" if first in ("reset", "") else f'mode "{first}"'
    if name == "dpms":
        state = "off" if first == "off" else "toggle" if first == "toggle" else "on"
        return f"output * power {state}"
    if name == "togglespecialworkspace":
        return "scratchpad show"
    if name in ("workspace",):
        if first.startswith("special"):
            return "scratchpad show"
        return workspace_target(first) or ""
    if name in ("movetoworkspace", "movetoworkspacesilent"):
        target, _, selector = arg.partition(",")
        crit = criteria(selector.strip()) if selector else ""
        if target.startswith("special"):
            return f"{crit} move scratchpad".strip()
        ws = workspace_target(target.strip())
        if not ws:
            return ""
        move = f"{crit} move container to {ws}".strip()
        return move if name.endswith("silent") or crit else f"{move}; {ws}"
    if name == "resizeactive":
        parts = arg.split()
        if parts and parts[0] == "exact" and len(parts) >= 3:
            def size(v):
                return f"{v[:-1]} ppt" if v.endswith("%") else f"{v} px"
            return f"resize set width {size(parts[1])} height {size(parts[2])}"
        nums = [int(float(p)) for p in parts if re.fullmatch(r"-?\d+(\.\d+)?", p)]
        if not nums:
            return ""
        dx, dy = (nums + [0])[:2]
        cmds = []
        if dx:
            cmds.append(f"resize {'grow' if dx > 0 else 'shrink'} width {abs(dx)} px")
        if dy:
            cmds.append(f"resize {'grow' if dy > 0 else 'shrink'} height {abs(dy)} px")
        return "; ".join(cmds)
    if name in ("moveactive",):
        nums = [int(float(p)) for p in arg.split() if re.fullmatch(r"-?\d+(\.\d+)?", p)]
        if len(nums) == 2:
            return f"move position {nums[0]} px {nums[1]} px" if arg.startswith("exact") else \
                "; ".join(c for c in (f"move {'right' if nums[0] > 0 else 'left'} {abs(nums[0])} px" if nums[0] else "",
                                       f"move {'down' if nums[1] > 0 else 'up'} {abs(nums[1])} px" if nums[1] else "") if c)
        return ""
    if name == "global":
        # Caelestia's global shortcuts have IPC twins on the desktop's own shell.
        _, _, action = arg.partition(":")
        if action:
            return f"exec caelestia shell hypr {action}"
        return ""
    # pseudo, setprop, focusmonitor and the rest have no sway match on one output.
    return ""


def run_dispatch(name, arg):
    command = dispatch(name, arg)
    if not command:
        return 0
    code, out = sway(command)
    if code != 0:
        sys.stderr.write(out)
    return code


def keyword(rest):
    name, _, value = rest.partition(" ")
    if name == "monitor":
        fields = [f.strip() for f in value.split(",")]
        if "transform" in fields:
            t = fields[fields.index("transform") + 1] if fields.index("transform") + 1 < len(fields) else "0"
            return sway(f"output * transform {TRANSFORMS.get(t, 'normal')}")[0]
        if len(fields) > 1 and fields[1] == "disable":
            return sway("output * power off")[0]
        return 0
    # Look and feel keywords (cursor zoom, blur, gaps) have no effect on the desktop.
    return 0


def host(args):
    env = dict(os.environ, HYPRLAND_INSTANCE_SIGNATURE=HOST_SIGNATURE)
    real = next((p for p in ("/usr/bin/hyprctl", "/usr/local/bin/hyprctl") if os.path.exists(p)), None)
    if not real or HOST_SIGNATURE.startswith("__"):
        return 1
    return subprocess.run([real] + args, env=env).returncode


def main(argv):
    as_json = False
    batch = None
    rest = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a in ("-j", "--json"):
            as_json = True
        elif a in ("-r", "--refresh", "-q", "--quiet"):
            pass
        elif a in ("--batch", "-b"):
            batch = argv[i + 1] if i + 1 < len(argv) else ""
            i += 1
        elif a in ("-i", "--instance"):
            i += 1
        else:
            rest.append(a)
        i += 1
    if batch is not None:
        code = 0
        for part in filter(None, (p.strip() for p in batch.split(";"))):
            code |= main(part.split(" ", 2) if part.startswith("dispatch") else part.split())
        return code
    if not rest:
        return 0
    what = rest[0]
    if what == "dispatch":
        return run_dispatch(rest[1] if len(rest) > 1 else "", " ".join(rest[2:]))
    if what == "keyword":
        return keyword(" ".join(rest[1:]))
    if what == "reload":
        return sway("reload")[0]
    if what in HOST_READ_ONLY:
        return host((["-j"] if as_json else []) + rest)
    handled = answer(what, as_json)
    return 0 if handled is None else handled


def ipc_reply(request):
    """Answer one Hyprland socket request, such as `j/monitors` or `[[BATCH]]a;b`."""
    import contextlib
    import io
    parts = request[len("[[BATCH]]"):].split(";") if request.startswith("[[BATCH]]") else [request]
    out = []
    for part in (p.strip() for p in parts):
        if not part:
            continue
        flags, _, command = part.partition("/") if re.match(r"^[a-z]*/", part) else ("", "", part)
        words = command.split(" ", 2)
        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            if words[0] == "dispatch":
                code = run_dispatch(words[1] if len(words) > 1 else "", words[2] if len(words) > 2 else "")
                print("ok" if code == 0 else "error")
            elif words[0] == "keyword":
                keyword(" ".join(words[1:]))
                print("ok")
            elif words[0] == "reload":
                sway("reload")
                print("ok")
            elif answer(words[0], "j" in flags) is None:
                print("[]" if "j" in flags else "unknown request")
        out.append(buffer.getvalue().rstrip("\n"))
    return "\n\n".join(out)


def serve(signature):
    import socket
    import threading
    base = os.path.join(os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "hypr", signature)
    os.makedirs(base, exist_ok=True)
    requests_path, events_path = os.path.join(base, ".socket.sock"), os.path.join(base, ".socket2.sock")
    listeners = []

    def listen(path):
        if os.path.exists(path):
            os.unlink(path)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.bind(path)
        sock.listen(16)
        return sock

    def handle(conn):
        with conn:
            conn.settimeout(2)
            data = b""
            try:
                while not data.endswith(b"\0"):
                    chunk = conn.recv(65536)
                    if not chunk:
                        break
                    data += chunk
                    if len(chunk) < 65536:
                        break
                try:
                    reply = ipc_reply(data.rstrip(b"\0").decode(errors="replace").strip())
                except Exception as error:  # a reply always goes back, so a client never hangs
                    reply = f"error: {error}"
                conn.sendall(reply.encode())
            except OSError:
                pass

    def requests_loop(sock):
        while True:
            conn, _ = sock.accept()
            threading.Thread(target=handle, args=(conn,), daemon=True).start()

    def events_loop(sock):
        while True:
            conn, _ = sock.accept()
            listeners.append(conn)

    def emit(lines):
        data = "".join(line + "\n" for line in lines).encode()
        for conn in list(listeners):
            try:
                conn.sendall(data)
            except OSError:
                listeners.remove(conn)

    def stop():
        for path in (requests_path, events_path):
            try:
                os.unlink(path)
            except OSError:
                pass
        try:
            os.rmdir(base)
        except OSError:
            pass
        os._exit(0)

    def sway_events():
        # sway detaches what it starts, so this process outlives a stopped desktop
        # unless it leaves when the desktop's sway socket goes.
        import time
        swaysock = os.environ.get("SWAYSOCK", "")
        while True:
            if swaysock and not os.path.exists(swaysock):
                stop()
            proc = subprocess.Popen(["swaymsg", "-r", "-m", "-t", "subscribe", '["workspace","window"]'],
                                    stdout=subprocess.PIPE, text=True)
            decoder, buffer = json.JSONDecoder(), ""
            for line in proc.stdout:
                buffer += line
                try:
                    event, end = decoder.raw_decode(buffer.lstrip())
                    buffer = buffer.lstrip()[end:]
                except ValueError:
                    continue
                change, current = event.get("change"), event.get("current") or {}
                if "container" in event:
                    con = event["container"]
                    address, app = hex(con.get("id", 0))[2:], con.get("app_id") or ""
                    if change == "focus":
                        emit([f"activewindow>>{app},{con.get('name') or ''}", f"activewindowv2>>{address}"])
                    elif change == "new":
                        emit([f"openwindow>>{address},,{app},{con.get('name') or ''}"])
                    elif change == "close":
                        emit([f"closewindow>>{address}"])
                    elif change == "title":
                        emit([f"windowtitle>>{address}", f"windowtitlev2>>{address},{con.get('name') or ''}"])
                elif change in ("focus", "init"):
                    name, num, output = current.get("name", ""), current.get("num", -1), current.get("output", "")
                    emit([f"workspace>>{name}", f"workspacev2>>{num},{name}",
                          f"focusedmon>>{output},{name}", f"focusedmonv2>>{output},{num}"])
                    if change == "init":
                        emit([f"createworkspace>>{name}", f"createworkspacev2>>{num},{name}"])
                elif change == "empty":
                    name, num = current.get("name", ""), current.get("num", -1)
                    emit([f"destroyworkspace>>{name}", f"destroyworkspacev2>>{num},{name}"])
            proc.wait()
            time.sleep(1)

    # Open the sockets only once sway answers with its outputs. Quickshell asks for
    # the monitors once, on connect, and never asks again.
    import time
    for _ in range(100):
        if query("get_outputs"):
            break
        time.sleep(0.1)
    for target, path in ((requests_loop, requests_path), (events_loop, events_path)):
        sock = listen(path)
        threading.Thread(target=target, args=(sock,), daemon=True).start()
    sway_events()


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--serve-ipc":
        serve(sys.argv[2])
    sys.exit(main(sys.argv[1:]))
