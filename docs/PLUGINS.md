# Plugins

A plugin replaces the text reader of chibipop. chibipop sends the plugin a picture of the screen. The plugin sends back the text and the position of each word.

Plugins work on Windows only. The Linux build always uses its bundled meikiocr engine.

Only plugins with the role `text-provider` do work today. The manifest also accepts `field-contributor`, but chibipop does not run that role.

> A plugin is a separate program. It runs with your full user rights. Add only plugins that you trust.

## Add a plugin

1. Copy the plugin folder into `plugins/`, next to `chibipop.exe`. Each plugin has its own folder.
2. Check that chibipop sees it:

   ```powershell
   .\chibipop.exe plugin list
   ```

   Each line shows the name, version, protocol, and roles. A broken plugin shows `REFUSED` and the reason.
3. Open the settings window. On the **Extensions** tab, tick **Enable** beside the plugin.
4. On the **Text recognition** tab, choose the plugin under **Text reader**.
5. Click **Apply**. The new reader works at once, without a restart.

If the plugin cannot start, chibipop uses Windows OCR and writes the reason to its log. **Show live logs** on the **Debug** tab opens the log.

The bundled [`plugins/meikiocr/`](../plugins/meikiocr/) plugin needs Python and extra setup. The [guide](GUIDE.md#9-changing-the-ocr-engine) explains it.

## Make a plugin

A plugin is a folder in `plugins/` with two parts:

```text
plugins/hello/
  plugin.toml    the manifest
  main.py        any program that speaks the protocol
```

The program can use any language. chibipop starts it with the plugin folder as the working directory.

### The manifest

`plugin.toml` describes the plugin.

| Key | Meaning | Default |
|---|---|---|
| `name` | The name of the plugin. Not empty. | required |
| `version` | Your version text. | required |
| `protocol` | The protocol number. It must be `1`. | required |
| `command` | The program to run. chibipop finds it on `PATH`. | required |
| `args` | The arguments for the program. | none |
| `roles` | The roles of the plugin. Use `["text-provider"]`. | required |

A text provider also needs a `[text_provider]` section:

| Key | Meaning | Default |
|---|---|---|
| `provides_geometry` | `true` if every line carries word boxes. Hover lookup needs this. | `false` |
| `languages` | The language tags. chibipop uses the first one. | none |
| `timeout_ms` | The time limit for one `text/recognise` request. | `500` |

chibipop refuses a manifest that has an unsupported protocol, no roles, an empty name, or a text-provider role without a `[text_provider]` section.

### The protocol

The plugin talks to chibipop with lines of JSON.

- chibipop writes one request on each line of the plugin's standard input.
- The plugin writes one reply on each line of its standard output.
- The plugin writes logs to standard error only. chibipop shows them in its log.
- Both sides ignore unknown fields.
- chibipop keeps at most one request waiting. A newer request replaces an older request that the plugin has not read.

A request looks like this:

```json
{"id": 1, "method": "hello", "params": {}}
```

A reply has the same `id`, and either a `result` or an `error`:

```json
{"id": 1, "result": {}}
{"id": 1, "error": "unknown method"}
```

An unknown method must get an `error` reply. The plugin must never crash on it.

#### `hello`

chibipop sends `hello` once, when it starts the plugin. The plugin has 5 seconds to reply. Load slow resources here, not on the first lookup.

The request `params` have these fields:

| Field | Meaning |
|---|---|
| `chibipop` | The chibipop version. |
| `protocol_supported` | The protocol numbers that chibipop supports. Today: `[1]`. |
| `features` | Reserved. Empty today. |
| `roles_wanted` | The roles from the manifest. |

The `result` must have these fields:

| Field | Meaning |
|---|---|
| `protocol` | The protocol that the plugin picked. It must match the manifest. |
| `name` | The name of the plugin. |
| `version` | The version of the plugin. |
| `roles` | The roles of the plugin. |
| `capabilities` | `{"geometry": true, "languages": ["ja"]}` |

#### `text/recognise`

chibipop sends this request for each lookup.

| Field | Meaning |
|---|---|
| `image_png` | A PNG picture, encoded as base64. |
| `region` | `{x, y, w, h}` of the picture. It starts at `0, 0`. |
| `scale` | Always `1` today. |
| `language` | The first language of the manifest. |
| `deadline_ms` | The time limit. It equals `timeout_ms`. |

The `result` holds the lines that the plugin read:

```json
{"lines": [
  {"text": "宿舎に",
   "words": [
     {"text": "宿舎", "rect": {"x": 0, "y": 0, "w": 112, "h": 60}},
     {"text": "に",   "rect": {"x": 112, "y": 0, "w": 30, "h": 60}}
   ]}
]}
```

- Each `rect` uses whole pixels. It is relative to the picture. The origin is the top-left corner. Do not add an offset.
- With `provides_geometry = true`, send `words` for each line. A line with text and no words cannot produce a hover match.
- meikiocr sends one word for each character.
- An empty `lines` list is a valid answer for a blank picture.

#### Failures

A request fails when the plugin replies with an `error`, sends invalid JSON, exits, or misses the time limit. After three failures in a row, chibipop stops using the plugin until the next restart. One good reply resets the count. When chibipop stops using a plugin, it ends the plugin and every program that the plugin started.

## Test a plugin

Save any picture as `test.png`. Then run:

```powershell
.\chibipop.exe plugin test hello --image test.png
```

The command starts the plugin, runs the handshake, and sends one `text/recognise` request. It prints the lines that came back:

```text
handshake ok in 97.6267ms: hello
recognise ok in 152.3µs, 1 line(s)
  line 0: "宿舎"  words 1
```

The command exits with `1` when a step fails. It also exits with `1` and prints `VIOLATION` when the manifest claims geometry and the reply has no words.

## A complete example

This plugin always returns the word 宿舎. It shows the whole protocol. Replace the fixed answer with a real OCR call.

`plugins/hello/plugin.toml`:

```toml
name = "hello"
version = "0.1.0"
protocol = 1
command = "python"
args = ["-u", "main.py"]
roles = ["text-provider"]

[text_provider]
provides_geometry = true
languages = ["ja"]
timeout_ms = 1500
```

`plugins/hello/main.py`:

```python
import json
import sys


def reply(request_id, result):
    sys.stdout.write(json.dumps({"id": request_id, "result": result}) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    request = json.loads(line)
    if request["method"] == "hello":
        reply(request["id"], {
            "protocol": 1,
            "name": "hello",
            "version": "0.1.0",
            "roles": ["text-provider"],
            "capabilities": {"geometry": True, "languages": ["ja"]},
        })
    elif request["method"] == "text/recognise":
        reply(request["id"], {"lines": [{
            "text": "宿舎",
            "words": [{"text": "宿舎", "rect": {"x": 0, "y": 0, "w": 112, "h": 60}}],
        }]})
    else:
        sys.stdout.write(json.dumps({"id": request["id"], "error": "unknown method"}) + "\n")
        sys.stdout.flush()
```

## Tips

- Flush standard output after every reply. A buffered reply looks like a missed time limit. In Python, use `python -u` and `flush()`.
- Never print anything else to standard output. It carries the protocol.
- Keep paths and thread counts for one machine in a config file beside the plugin. The meikiocr plugin does this in `config.toml`.
- Read [`plugins/meikiocr/adapter.py`](../plugins/meikiocr/adapter.py) for a full, working plugin.
