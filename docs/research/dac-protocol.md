# Eight Sleep Pod 3 — `dac` Application Protocol Spec

Reverse-engineered from readable TypeScript source of `@eight/device-api-client`
(the `dac` app) in the stock rootfs at `home/dac/app/src`.

`dac` = "Device API Client". Internally the runtime object is nicknamed
**PizzaRat** and the local hardware firmware it talks to is **frankenfirmware**
("franken"). The persistent-info directory helper is called **Sewer**.

`dac` sits in the middle of two protocols:

```
  frankenfirmware  <--(unix socket, newline framing)-->  dac  <--(CoAP/TLS/TCP)-->  device-api.8slp.net
   (local hardware controller)                     PizzaRat                    (Eight Sleep cloud)
```

`dac` is essentially a **dumb bridge / proxy**. It holds almost no logic of its
own: the cloud (`device-api`) drives everything by requesting variables and
calling functions, and `dac` forwards those to frankenfirmware and relays
frankenfirmware's telemetry back up. See §5 for what that means for a
standalone FOSS replacement.

---

## 0. Process startup / config

`src/main.ts`:

```ts
const host   = config.get("serverHost");     // device-api.8slp.net
const port   = config.get("serverPort");     // 5684
const sewerPath = config.get("sewerPath");    // /deviceinfo/   (NODE_ENV=pod3)
const socketPath = path.join(sewerPath, "dac.sock");   // /deviceinfo/dac.sock
const serverCert = readFile(config.get("serverCert"));  // ./dev_server_1.crt

const sewer = Sewer.fromPath(sewerPath);
const hwInfo = await DeviceInfo.fromSewer(sewer, serverCert);
const deviceApi = new DeviceApi(hwInfo.deviceId, {host,port}, hwInfo, logger);
const frankenServer = await FrankenServer.start(socketPath, logger);  // unix socket LISTEN
const pizzaRat = new PizzaRat(frankenServer, deviceApi, hwInfo, logger);
await pizzaRat.run();
```

### Config (`config/` npm `config` module, keyed by `NODE_ENV`)

`config/default.json`:
```json
{
    "logging": { "name": "device-api-client" },
    "deviceType": "pizza1",
    "serverHost": "device-api.8slp.net",
    "serverPort": 5684,
    "sewerPath": "/sewer/",
    "serverCert": "./dev_server_1.crt",
    "privateKey": "test_device_1.key",
    "cert": "test_device_1.crt",
    "variableFetchIntervalMs": 5000
}
```
`config/pod3.json` (production on the Pod, `run.sh` sets `NODE_ENV=pod3`):
```json
{ "sewerPath": "/deviceinfo/" }
```
`config/development.json`: `{ "sewerPath": "./sewer/" }`

So on device:
- **Cloud endpoint: `device-api.8slp.net:5684`** (5684 is the IANA CoAPS port; here
  it is CoAP-over-**TLS/TCP**, not DTLS — see §2).
- **Local unix socket: `/deviceinfo/dac.sock`** (dac is the *server*/listener;
  frankenfirmware is the *client* that connects in). Note: task brief says
  `DAC_SOCKET` env var / `/deviceinfo/dac.sock`; in this source the path is built
  from `sewerPath + "dac.sock"`, no env var is read in `main.ts`. frankenfirmware
  presumably has `DAC_SOCKET=/deviceinfo/dac.sock` on its side.
- **Persistent device identity dir: `/deviceinfo/`** (the "sewer").

`run.sh` waits for `/sewer/device-id` to exist before launching, retries `npm
start` up to 10×. `watchdog.sh` kills node if the log file stops growing for 3
min (stall detection). (Note `run.sh` references `/sewer` but pod3 config uses
`/deviceinfo`; the guard path in run.sh appears stale.)

---

## 1. dac ↔ frankenfirmware LOCAL protocol (unix socket)

Files: `franken_server.ts`, `message_stream.ts`, `fake_franken.ts` (mock =
gold), `franken_tester.ts`, `utils.ts`. `message_buffer.ts` is an unused
alternate buffering helper (string-based, not wired into the socket path).

### 1.1 Transport & framing

- **Unix domain socket**, path `/deviceinfo/dac.sock`. `dac` calls
  `net.createServer().listen(path)` and waits for a `connection`
  (`FrankenServer.start` / `waitForFranken`). frankenfirmware connects as client.
- **Framing = message-oriented, delimiter `"\n\n"` (0x0A 0x0A)**. This is the
  single most important local-protocol fact.

`franken_server.ts`:
```ts
static readonly separator = Buffer.from("\n\n");
```
`message_stream.ts` (`MessageStream.readMessage`) accumulates bytes until it
finds `\n\n`, returns everything before it, and keeps the remainder buffered:
```ts
const index = this.buffer.indexOf(this.separator);   // "\n\n"
const message = this.buffer.slice(0, index);
this.buffer = this.buffer.slice(index + separator.length);
return message;
```

- **Request/response is strictly synchronous and half-duplex from dac's view**,
  serialized through `SequentialQueue`: dac writes one request framed with a
  trailing `\n\n`, then blocks reading exactly one `\n\n`-framed response before
  issuing the next. `sequential_queue.ts` chains promises so only one
  request/response is in flight at a time.

```ts
private async sendMessage(request: string) {
    return await this.sequentialQueue.exec(async () => {
        const requestBytes = Buffer.concat([Buffer.from(request), separator]); // req + "\n\n"
        await this.writeStream.write(requestBytes);
        return (await this.messageStream.readMessage()).toString();            // one framed resp
    });
}
```

### 1.2 Message payload format

Two request shapes, both plain ASCII:

**(a) Call a function** (`callFunction(name, arg)`):
```
<commandNumber>\n<arg>\n\n
```
i.e. the numeric opcode, a single `\n`, the argument string, then the `\n\n`
frame terminator. Response is read and ignored by dac (`callFunction` discards it).

**(b) Request all variables** (`getVariables()`):
```
14\n\n
```
just opcode `14` (PLEASE_SEND_VARIABLES) framed. Response is a **newline-separated
list of `key = value` lines**:
```
ipaddr = "127.0.0.1"
heatLevelL = 0
tgHeatLevelL = 1
...
```
parsed by:
```ts
Object.fromEntries(varResp.split("\n").map(l => l.split(" = ")))
```
Note the literal separator is `" = "` (space-equals-space). Values are the raw
strings (some are JSON-quoted, e.g. `ipaddr` value is `"127.0.0.1"` including quotes).

### 1.3 Opcode table (`utils.ts` `frankenCommands`)

| Opcode | Name                  | Meaning / arg |
|-------:|-----------------------|---------------|
| 0  | HELLO                  | handshake |
| 1  | SET_TEMP               | set temperature |
| 2  | SET_ALARM              | set alarm |
| 3  | RESET                  | reset |
| 4  | FORCE_RESET            | force reset |
| 5  | ALARM_LEFT             | left-side alarm |
| 6  | ALARM_RIGHT            | right-side alarm |
| 7  | FORMAT                 | format storage |
| 8  | SET_SETTINGS           | push settings blob (CBOR, see §4) |
| 9  | HEAT_LEFT              | left heating; arg = duration seconds (e.g. `"7200"`) |
| 10 | HEAT_RIGHT             | right heating; arg = duration seconds |
| 11 | LEVEL_LEFT             | left heating level (setpoint) |
| 12 | LEVEL_RIGHT            | right heating level (setpoint) |
| 13 | PRIME                  | prime the water pump |
| 14 | PLEASE_SEND_VARIABLES  | dump all variables |

Cloud-facing function names map to these opcodes via
`funcNameToFrankenCommand` (`utils.ts`):

| cloud func name | franken opcode name |
|-----------------|---------------------|
| `reset`       | RESET |
| `force-reset` | FORCE_RESET |
| `format`      | FORMAT |
| `alarmR`      | ALARM_RIGHT |
| `alarmL`      | ALARM_LEFT |
| `setsettings` | SET_SETTINGS |
| `prime`       | PRIME |
| `leftHeat`    | HEAT_LEFT |
| `leftLevel`   | LEVEL_LEFT |
| `rightHeat`   | HEAT_RIGHT |
| `rightLevel`  | LEVEL_RIGHT |

(Interesting: cloud never directly calls opcodes 0/1/2 by these names — HELLO/
SET_TEMP/SET_ALARM. Temperature is driven via `leftLevel`/`rightLevel` (opcodes
11/12) and heating duration via `leftHeat`/`rightHeat` (9/10). "alarmL/alarmR"
→ 5/6. `SET_TEMP` (1) and `SET_ALARM` (2) are not in the func map.)

### 1.4 Variable list (`utils.ts` `variableNames`) — telemetry frankenfirmware reports

Requested every ~1 s by `PizzaRat.frankenLoop` (see §1.6). Names dac understands
as valid variables (the `/v/<name>` cloud endpoint whitelist too):

```
ipaddr, sigstr, ssid, settings, updating, priming, waterLevel, sensorLabel,
heatLevelL, tgHeatLevelL, heatTimeL, heatLevelR, tgHeatLevelR, heatTimeR,
hubInfo, macAddr
```

Meaning (inferred; `L`/`R` = left/right bed side):
- `heatLevelL/R`   — current heating level (actual)
- `tgHeatLevelL/R` — **t**ar**g**et heating level (setpoint)
- `heatTimeL/R`    — remaining heat time / duration
- `waterLevel`     — water reservoir level
- `priming`        — pump priming in progress (0/1)
- `updating`       — firmware update in progress (0/1)
- `sensorLabel`    — sensor identifier
- `settings`       — CBOR-hex settings blob (see §4)
- `sigstr`/`ssid`/`ipaddr` — Wi-Fi signal, SSID, IP
- `hubInfo`        — overridden by dac to `deviceLabel`
- `macAddr`        — overridden by dac to hardware MAC

`describePayload()` (`utils.ts`) builds the `/d` describe response advertising
the function names (`f`) and variables (`v`, each valued `4`):
```ts
{ f: funcNames, v: { <each var>: 4, ... } }   // JSON
```

### 1.5 `fake_franken.ts` — mock of frankenfirmware (protocol reference)

Connects as a client (to `/tmp/sewerSocket` in the mock), reads `\n\n`-framed
messages, and:
- if message == `"14"` (PLEASE_SEND_VARIABLES) → replies with `var1 = 123`
  (`key = value` lines) `+ "\n\n"`.
- otherwise echoes `"resp:" + message + "\n\n"`.

Confirms: frankenfirmware = the socket **client**, dac = **server**; framing is
`\n\n`; variable response format is `key = value\n…`.

### 1.6 PizzaRat orchestration (`pizza_rat.ts`)

Runs two loops concurrently:
- **`frankenLoop`**: `waitForFranken()`, then every 1000 ms call
  `franken.getVariables()` and merge into `this.variableValues`. On error clears
  `this.franken` and waits for reconnect. Seeds with `defaultVarValues` (see §4).
- **`deviceApiLoop`**: connect to cloud `device-api`, register handlers, run;
  on disconnect wait 5 s and reconnect forever.

Cloud→franken bridging:
- `onVarRequest(name)`: cloud asks for a variable → return cached
  `variableValues[name]`, except `macAddr`→`JSON.stringify(hwInfo.macAddress)`,
  `hubInfo`→`JSON.stringify(hwInfo.deviceLabel)`.
- `onFuncRequest(name, arg)`: cloud calls a function → look up
  `funcNameToFrankenCommand[name]` → `franken.callFunction(cmd, arg)` (forwarded
  over the unix socket). If franken is disconnected, the call is dropped/throws.

---

## 2. dac ↔ cloud protocol (`device_api.ts`, `dev_api/*`)

### 2.1 Transport

- **CoAP messages (RFC 7252) carried over a TLS-over-TCP stream** to
  `device-api.8slp.net:5684`. Not DTLS/UDP — the code uses `tls.connect(port,
  host, options)` (`device_api.ts` `tlsConnect`), i.e. a TCP+TLS socket.
- TLS options (`credentialsToOptions`): `ca: [serverCert]`,
  **`rejectUnauthorized: false`**, and `checkServerIdentity` returns `undefined`
  (accept any). Client cert/key are **commented out** ("todo: comment out when we
  get real certs"). So mutual TLS is *not* actually enforced in this build — the
  server cert isn't verified and no client cert is sent.
- `serverCert` loaded from `./dev_server_1.crt` (a dev cert shipped in the app dir).

### 2.2 Handshake ("howdy") + framing

After TLS connect (`createSecureStream`):
1. dac reads exactly **128 bytes** (`howdySize`) — the server "howdy" greeting
   (contents currently unverified: "todo: verify howdy").
2. dac sends an **8-byte header + deviceId** identity frame:

```ts
// DeviceApi.formatHeader(messageSize, protocolVersion=1)
header = Buffer.alloc(8);
header.writeUInt16BE(0x8888, 0);   // magic
header.writeUInt16BE(0x0001, 2);   // protocol version = 1
header.writeUInt32BE(len,    4);   // length of following deviceId bytes
// then: header ++ Buffer.from(deviceId)
```
Byte layout of the identity frame:
```
offset  0   1   2   3   4   5   6   7   8 .. 8+len
       88  88  00  01  [ msgSize u32 BE ]  <deviceId ascii>
```
`deviceId` comes from `/deviceinfo/device-id` first line (`d1a000000001` in the
sample sewer; note the file's first line is actually a placeholder string
`this-is-a-really-long-id-new-format-is-long`, real id on line 2 — see §3 caveat).

3. **After the handshake, this framing header is NOT reused.** `CoapStream`
   (`dev_api/coap_stream.ts`) simply does `stream.write(message.toBuffer())` and
   `Message.fromBuffer(stream.read())`. Each raw CoAP message is written/read as
   one buffer with **no length prefix** — the code relies on one TCP read ≈ one
   CoAP message. (Fragile but that's the implementation.)

### 2.3 CoAP message format (standard RFC 7252, via `@eight/h5.coap` 0.0.2)

Confirmed in `node_modules/@eight/h5.coap/lib/Message.js` `toBuffer`/`fromBuffer`:
4-byte header `(version=1<<30) | (type<<28) | (tokenLen<<24) | (code<<16) | id`,
then token, then options (URI-Path, URI-Query, …), then `0xFF` payload marker,
then payload. Types CON/NON/ACK/RST; codes GET/POST/PUT + response codes
(CONTENT, CHANGED, …).

`DeviceProtocol` (`dev_api/device_protocol.ts`) implements a CoAP endpoint:
- generates message IDs (`currentId++ % 0xff`) and tokens (`currentToken++ % 0xff`);
- `sendMessage(request)`: for CON messages waits for ACK (`InFlight` keyed by id)
  and, if a response is needed, also waits for a response keyed by token hex;
- `run()`: read loop — ACKs resolve pending acks; messages with a token resolve
  pending responses; otherwise dispatch to a registered request handler by path
  prefix. `/` is a built-in ping/no-op.

### 2.4 Endpoints (CoAP URI paths)

Registered handlers dac exposes to the cloud (`DeviceApiClient` ctor):
- **`/h`** — "hello" ack; resolves the hello handshake promise.
- **`/d`** — **describe**: returns `describePayload()` (JSON of functions+vars).
- **`/v/<name>`** — **read variable**: cloud GETs a variable; dac returns
  `variableValues[name]` (validated against the whitelist; invalid → error).
- **`/f`** and **`/f/<name>?<arg>`** — **call function**: bare `/f` is treated as
  a FW-update trigger (responds `changed`); `/f/<name>` with a URI-query arg runs
  `funcHandler(name, arg)` → forwarded to franken. Responds `changed` +
  4 zero bytes.

Messages dac **sends** to the cloud:
- **`POST /h`** with hello payload (`sendHello`) — on connect.
  Payload (`helloPayload("pod3")`): `productId(2)="PR"` ++ `fwVersion(8)=0x03…` ++
  `fwCommit(7)="pod3…"`. Comment: "pod3 is a special commit — device-api will
  ignore [fw update]" (i.e. sending commit "pod3" disables OTA on the server).
- **`POST /`** every 10 s (`pinger` / `sendPing`) — keepalive.
- **`POST /E/tracing/rat`** with a text payload (`sendLog`) — remote log/trace
  channel ("rat" tracing).

Socket session timeout 60 s; `setNoDelay(true)`.

### 2.5 What flows

- **Cloud → device (commands):** read any variable (`/v/*`), invoke any function
  (`/f/<name>?<arg>`) — i.e. set heating level/target temp (`leftLevel`/
  `rightLevel`), heating duration (`leftHeat`/`rightHeat`), alarms
  (`alarmL`/`alarmR`), prime, reset, format, push settings (`setsettings`),
  trigger FW update (`/f`).
- **Device → cloud:** hello (identity + fw info), 10 s pings, log/trace lines.
  There is **no explicit periodic telemetry push in this build** — the cloud
  *pulls* sensor/heat state on demand via `/v/*`. Sleep-tracking sensor data as
  such is not modeled here beyond the `variableNames` list; heavy biometric/piezo
  data handling is not present in dac (likely handled elsewhere / not in this
  component). dac only relays the frankenfirmware variables.

---

## 3. SEWER / persistence (`sewer.ts`, `/deviceinfo/`)

"Sewer" is just a thin reader over the persistent identity directory
(`/deviceinfo/` on pod3). It is **read-only** in this code — dac does not write
buffered telemetry to disk here (no store-and-forward queue implemented; the only
buffering is the in-memory `variableValues` map and the `\n\n` MessageStream
byte buffer).

`Sewer`: `read(name)` = read `join(path,name)`; `readFirstLine(name)`.

`DeviceInfo.fromSewer` reads:
- **`device-id`** (first line, trimmed) → `deviceId`
- **`device-label`** (first line) → `deviceLabel`  (sample: `20500-0000-F00-00001234`)
- **`wifi-macaddress`** (first line) → `macAddress` (sample: `d8:5e:d3:07:11:3a`)
- `ec_ssid` → `readSsid()`; `ec_signal_strength` → `readSignalStrength()` (parseInt)
- `serverCert` is passed in (from `./dev_server_1.crt`), **not** read from sewer.
- `myPrivateKey` / `myCert` are hardcoded `"na"` (client TLS creds disabled).

Caveat: the sample `sewer/device-id` file has two lines
(`this-is-a-really-long-id-new-format-is-long` then `d1a000000001`); code takes
`readFirstLine`, so it would use the placeholder unless the real file's first
line is the id. On a real device the first line is the actual device id.

No `/persistent` path is referenced anywhere in dac; persistence for dac = the
`/deviceinfo/` identity files only.

---

## 4. Data model (fields / units / settings)

### 4.1 Heating / temperature

- Heating is expressed as a **level / "heat level"**, not degrees, on both the
  franken vars and the cloud funcs:
  - `heatLevelL/R` = current level, `tgHeatLevelL/R` = target level,
    `heatTimeL/R` = time.
  - Cloud funcs `leftLevel`/`rightLevel` (opcodes 11/12) set the level;
    `leftHeat`/`rightHeat` (9/10) take a **duration in seconds** (tester uses
    `HEAT_LEFT "7200"` = 2 h).
- The Eight Sleep "level" scale is app-facing roughly −100…+100 (cool↔warm);
  dac passes the arg string through verbatim to frankenfirmware, so the actual
  unit/scale conversion lives in frankenfirmware, not dac.

`defaultVarValues` (`pizza_rat.ts`) — seed values before franken connects:
```
sensorLabel: "null", waterLevel: "0", updating: "0", priming: "0",
sigstr: "0", ssid: "0", ipaddr: '"127.0.0.1"', hubInfo: '""',
settings: '"BF61760162676C19029C62677219029C626C621864FF"',
heatLevelL: "0", heatLevelR: "0",
tgHeatLevelL: "1", tgHeatLevelR: "1",
heatTimeL: "2", heatTimeR: "2"
```

### 4.2 Settings blob (CBOR)

`settings` var / `SET_SETTINGS` (opcode 8) / cloud `setsettings` carry a
**CBOR-encoded map, hex-stringified**. Decoding the default
`BF61760162676C19029C62677219029C626C621864FF`:
```
BF                     map(*)  (indefinite)
  61 76        "v"  -> 01            v   = 1        (schema/version)
  62 67 6C     "gl" -> 19 029C = 668 gl  = 668      (goal left)
  62 67 72     "gr" -> 19 029C = 668 gr  = 668      (goal right)
  62 6C 62     "lb" -> 18 64  = 100  lb  = 100      (level brightness? / LED)
FF                     break
```
→ `{ v:1, gl:668, gr:668, lb:100 }`. `gl`/`gr` = per-side goal/target
(temperature or level, value 668), `lb` = a 0–100 setting (brightness/level).
Exact unit of 668 is defined by frankenfirmware.

### 4.3 Identity

- `deviceId` (e.g. `d1a000000001`) — used in the cloud handshake identity frame.
- `deviceLabel` (`20500-0000-F00-00001234`) — reported to cloud as `hubInfo`.
- `macAddress` (`d8:5e:d3:07:11:3a`) — reported as `macAddr`.

---

## 5. What must be reimplemented for a standalone (no-cloud) device

`dac`'s responsibilities, split by cloud dependence:

### Purely local (must keep / reimplement locally)
1. **Unix-socket server on `/deviceinfo/dac.sock`** speaking the `\n\n`-framed
   text protocol (§1): accept frankenfirmware's connection, serialize
   request/response through a single-in-flight queue.
2. **Variable polling loop** — every ~1 s send `14\n\n`, parse `key = value`
   lines, cache them. This is the local telemetry source (heat levels, water
   level, priming, Wi-Fi, settings).
3. **Function dispatch** — build `<opcode>\n<arg>\n\n` frames to set heating
   level (11/12), heating duration (9/10), alarms (5/6), prime (13), settings (8),
   reset (3/4), format (7). This is the entire local control surface.
4. **Read device identity** from `/deviceinfo/` (device-id, device-label,
   wifi-macaddress). Only needed if you keep any identity semantics.

### Cloud-dependent (must be REPLACED by local logic to run standalone)
The whole `device_api.ts` + `dev_api/*` CoAP client and everything it enables:
5. **The scheduler / decision-maker.** In stock firmware the *cloud* decides
   when to heat, to what level, for how long, and when alarms fire — it does so
   by calling `/f/leftLevel`, `/f/leftHeat`, `/f/alarmL`, etc. dac itself has **no
   scheduling, no thermostat control loop, no alarm logic, no sleep-stage logic**.
   A FOSS standalone replacement must implement locally:
   - a **thermostat/heat controller** (choose level & duration per side, react to
     `heatLevelL/R` vs `tgHeatLevelL/R`, water level, priming),
   - a **schedule engine** (bed times, temperature curves over the night),
   - **alarm scheduling** (fire ALARM_LEFT/RIGHT at target times, incl. vibration/
     thermal wake),
   - **settings management** (build the CBOR settings blob for `SET_SETTINGS`).
6. **Remote logging / tracing** (`/E/tracing/rat`) — drop or send to local logs.
7. **OTA update trigger** (`/f`) — drop or replace with a local updater.
8. **Cloud identity/TLS handshake** (howdy + `0x8888` frame, hello, ping) — drop
   entirely.

### Net
A standalone replacement can be built as a program that:
- opens the `/deviceinfo/dac.sock` unix server, does the `\n\n` sync protocol,
- polls variables and issues functions per the opcode table,
- and adds a **local control plane** (thermostat + schedule + alarm UI/API)
  where the cloud used to be.

The local frankenfirmware protocol is small, text-based, and fully documented
above — it is the only piece that must be spoken exactly. Everything on the
cloud side can be discarded and replaced with local logic.

---

## Appendix — key file map

| File | Role |
|------|------|
| `src/main.ts` | wiring / config / startup |
| `src/pizza_rat.ts` | orchestrator: bridges franken vars/funcs ↔ cloud |
| `src/franken_server.ts` | unix-socket server; `\n\n` framed sync req/resp; opcode calls |
| `src/message_stream.ts` | `\n\n` delimiter buffering/reassembly |
| `src/message_buffer.ts` | (unused) alt string message buffer |
| `src/fake_franken.ts` | mock frankenfirmware (protocol reference) |
| `src/franken_tester.ts` | manual test harness (getVariables + HEAT_LEFT) |
| `src/utils.ts` | opcode table, variable names, func-name map, describe payload |
| `src/sewer.ts` | read `/deviceinfo/` identity files |
| `src/device_api.ts` | cloud TLS/TCP connect, howdy+0x8888 handshake, CoAP client/handlers |
| `src/dev_api/device_protocol.ts` | CoAP CON/ACK/token protocol engine |
| `src/dev_api/coap.ts` | request/response ↔ CoAP Message mapping |
| `src/dev_api/coap_stream.ts` | CoAP Message ↔ raw buffer over TLS stream |
| `src/dev_api/in_flight.ts` | pending ack/response promise map w/ timeout |
| `config/*.json` | serverHost/port, sewerPath by NODE_ENV |
| `node_modules/@eight/h5.coap` | RFC 7252 CoAP message codec |
