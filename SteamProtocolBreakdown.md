# Steam Content Delivery Protocol — Implementation Reference

A step-by-step breakdown of the Steam authentication and content download protocol as reverse-engineered by the SteamRE community and implemented in SteamKit2/DepotDownloader. Written as a reference for building an alternative client in Rust.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Connection: CM Server Discovery](#2-connection-cm-server-discovery)
3. [Authentication: Anonymous Login](#3-authentication-anonymous-login)
4. [Authentication: Credential-Based Login](#4-authentication-credential-based-login)
5. [Authentication: Token Persistence & Reuse](#5-authentication-token-persistence--reuse)
6. [Session State: Licenses & Entitlements](#6-session-state-licenses--entitlements)
7. [App Info: Resolving Depots & Manifests](#7-app-info-resolving-depots--manifests)
8. [Depot Key: Obtaining Decryption Keys](#8-depot-key-obtaining-decryption-keys)
9. [Manifest Request Code: Authorization](#9-manifest-request-code-authorization)
10. [CDN Server Selection](#10-cdn-server-selection)
11. [CDN Auth Tokens](#11-cdn-auth-tokens)
12. [Manifest Download & Parsing](#12-manifest-download--parsing)
13. [Delta Comparison: Old vs New Manifest](#13-delta-comparison-old-vs-new-manifest)
14. [Chunk Download Pipeline](#14-chunk-download-pipeline)
15. [Chunk Processing: Decrypt, Decompress, Verify](#15-chunk-processing-decrypt-decompress-verify)
16. [File Assembly & Finalization](#16-file-assembly--finalization)
17. [Connection Recovery & Error Handling](#17-connection-recovery--error-handling)
18. [Concurrency Considerations](#18-concurrency-considerations)
19. [Key Data Structures](#19-key-data-structures)
20. [Relevant Crates for Rust Implementation](#20-relevant-crates-for-rust-implementation)

---

## 1. Architecture Overview

The Steam content delivery system has two distinct communication layers:

```
┌─────────────────────────────────────────────────────┐
│                    Your Client                       │
├──────────────────────┬──────────────────────────────┤
│   CM Connection      │     CDN Downloads            │
│   (Stateful)         │     (Stateless)              │
│                      │                              │
│   - WebSocket/TCP    │     - HTTPS GET              │
│   - Protobuf msgs    │     - No session state       │
│   - Auth, app info   │     - Manifest + chunk fetch │
│   - Depot keys       │     - Auth tokens in headers │
│   - Manifest codes   │     - Freely parallelizable  │
└──────────┬───────────┴──────────────┬───────────────┘
           │                          │
           ▼                          ▼
   Steam CM Servers            Steam CDN Servers
   (cm0-*.steamserver.net)     (*.steamcontent.com)
```

**CM (Connection Manager)** servers handle all authenticated/stateful operations. You maintain a single persistent connection for the lifetime of your session.

**CDN servers** are stateless HTTP endpoints. You hit them for manifest and chunk downloads. They are geographically distributed, cacheable, and you can hit multiple in parallel.

---

## 2. Connection: CM Server Discovery

Before doing anything, you need to find a CM server to connect to.

### Step 2.1 — Fetch CM Server List

Make an HTTP GET request to Steam's WebAPI:

```
GET https://api.steampowered.com/ISteamDirectory/GetCMListForConnect/v1/?cellid=0
```

**Response**: A JSON object containing arrays of WebSocket and TCP server addresses, each with geographic info and load weighting.

The `cellid` parameter represents your geographic region. Pass `0` for auto-detection; Steam will suggest the best CellID in the login response.

### Step 2.2 — Connect to a CM Server

Establish a **WebSocket** connection (preferred) or **TCP** connection to one of the returned servers.

- **WebSocket**: `wss://cm0-xxx.steamserver.net:443/cmsocket/`
- **TCP**: Raw TCP on port 27017-27019, using a custom framing protocol with encrypted envelope

SteamKit2 defaults to WebSocket. The connection is encrypted using a key exchange during the handshake (for TCP) or via TLS (for WebSocket).

### Step 2.3 — Connection Protocol

Once connected, all communication uses Steam's protobuf-based message protocol:

- Each message has an **EMsg** identifier (uint32 enum, ~4000+ values)
- Messages are wrapped in a `CMsgProtoBufHeader` containing: `steamid`, `sessionid`, `jobid_source`, `jobid_target`
- The body is a protobuf message specific to that EMsg

---

## 3. Authentication: Anonymous Login

The simplest path. Used for dedicated server content that's freely available.

### Step 3.1 — Send Anonymous LogOn

Send `EMsg.ClientLogon` with an anonymous logon protobuf:

```protobuf
CMsgClientLogon {
    protocol_version: <current_steam_protocol_version>
    client_os_type: <your_os>
    // No username, password, or token fields
}
```

The SteamKit2 method is `steamUser.LogOnAnonymous()`.

### Step 3.2 — Receive LogOn Response

Steam responds with `EMsg.ClientLogOnResponse`:

```protobuf
CMsgClientLogonResponse {
    eresult: 1  // EResult.OK
    cell_id: 35 // Your geographic cell
    // ... heartbeat interval, vanity URL, etc.
}
```

### Step 3.3 — Start Heartbeats

After successful logon, you must send periodic heartbeat messages (`EMsg.ClientHeartBeat`) at the interval specified in the logon response (typically every 30 seconds). Failure to heartbeat will result in disconnection.

### Step 3.4 — Receive License List

Immediately after logon, Steam sends `EMsg.ClientLicenseList` containing the licenses available to the anonymous account. For anonymous, this is **Sub ID 17906** (the anonymous dedicated server subscription), which grants access to all free dedicated server apps.

---

## 4. Authentication: Credential-Based Login

Required for content that isn't freely available to anonymous accounts.

### Step 4.1 — Check for Stored Token

Before prompting for credentials, check if a **refresh token** from a previous session exists in your persistent storage for this username. If so, skip to [Step 5](#5-authentication-token-persistence--reuse).

### Step 4.2 — Begin Auth Session

SteamKit2's newer auth flow uses `BeginAuthSessionViaCredentials`:

```protobuf
CAuthentication_BeginAuthSessionViaCredentials_Request {
    device_friendly_name: "YourApp"
    account_name: "username"
    encrypted_password: <RSA-encrypted password>
    encryption_timestamp: <from GetPasswordRSAPublicKey>
    persistence: 1  // PERSISTENT if remember-password
    platform_type: <your_platform>
}
```

**Password encryption**: Before sending, you must:
1. Call `CAuthentication_GetPasswordRSAPublicKey` to get the RSA public key and timestamp for this account
2. RSA-encrypt the plaintext password with that key
3. Send the encrypted password + timestamp

### Step 4.3 — Handle Steam Guard Challenges

The auth session response may require additional verification:

| Challenge Type | Trigger | Action |
|---|---|---|
| **Email code** | Account has email-based Steam Guard | Prompt user, submit code via `UpdateAuthSessionWithSteamGuardCode` |
| **2FA TOTP code** | Account has mobile authenticator | Prompt user, submit code via `UpdateAuthSessionWithSteamGuardCode` |
| **Mobile confirmation** | Account has mobile authenticator | Wait for user to confirm on Steam Mobile App |

### Step 4.4 — Poll for Auth Result

After submitting the challenge response, poll `PollAuthSessionStatus` until it returns a result:

```protobuf
CAuthentication_PollAuthSessionStatus_Response {
    refresh_token: "eyAid..." // JWT refresh token
    access_token: "eyAid..."  // JWT access token
    account_name: "username"
    new_guard_data: "..."     // Machine auth token for future logins
}
```

### Step 4.5 — LogOn with Access Token

Now perform the actual CM logon using the access token:

```protobuf
CMsgClientLogon {
    protocol_version: <current>
    client_os_type: <your_os>
    account_name: "username"
    access_token: "eyAid..." // The token from Step 4.4
    should_remember_password: true
    login_id: <unique_uint32> // Important for concurrent sessions
}
```

### Step 4.6 — Receive LogOn Response + Licenses

Same as anonymous: receive `CMsgClientLogonResponse` and `ClientLicenseList`, but now with the full set of licenses owned by the account.

---

## 5. Authentication: Token Persistence & Reuse

When `-remember-password` is used, the refresh token from Step 4.4 is stored locally.

### Step 5.1 — Store Tokens

After successful auth, persist:
- `refresh_token` → keyed by username
- `guard_data` (machine auth token) → keyed by username

DepotDownloader stores these in `account.config` using .NET's IsolatedStorage, compressed with DeflateStream.

### Step 5.2 — Reuse on Next Run

On subsequent runs:
1. Look up stored `refresh_token` for the given username
2. Set `logonDetails.AccessToken = stored_token`
3. Call `steamUser.LogOn(logonDetails)` directly — no auth session needed
4. Pass stored `guard_data` to skip Steam Guard challenges

### Step 5.3 — Handle Token Rejection

If the logon response returns `EResult.InvalidPassword`, `InvalidSignature`, `AccessDenied`, `Expired`, or `Revoked`:
1. Remove the stored token for this username
2. Abort or fall back to credential-based auth

---

## 6. Session State: Licenses & Entitlements

After login, you need to verify the account has access to the content you want to download.

### Step 6.1 — Parse License List

The `ClientLicenseList` callback contains an array of `License` objects, each representing a Steam package (subscription). Each license references a **Package ID** (Sub ID).

### Step 6.2 — Get Package Info

For each relevant license, call `steamApps.PICSGetProductInfo()` with the package IDs. The response contains KeyValue data describing what app IDs and depot IDs each package grants access to:

```
"packageid" {
    "appids" {
        "0" "730"     // CS2
        "1" "731"     // CS2 Depot
    }
    "depotids" {
        "0" "731"
        "1" "732"
    }
}
```

### Step 6.3 — Verify Entitlement

Before downloading, confirm the target depot ID appears in at least one of the account's licensed packages. If not, the download will fail at the depot key request stage.

For free-on-demand content, DepotDownloader attempts to call `steamApps.RequestFreeLicense(appId)` to automatically acquire the license.

---

## 7. App Info: Resolving Depots & Manifests

This is where you figure out *what* to download.

### Step 7.1 — Request App Info

Call `steamApps.PICSGetProductInfo()` with the target app ID. This returns a rich KeyValue tree describing the app:

```
"appinfo" {
    "common" { ... }
    "depots" {
        "731" {
            "config" {
                "oslist" "windows,linux"
            }
            "manifests" {
                "public" "7617088375292372759"  // manifest ID
                "beta_branch" "1234567890123456789"
            }
            "encryptedmanifests" {
                "password_protected_branch" { ... }
            }
            "depotfromapp" "740"  // Optional: depot proxied through another app
        }
    }
}
```

### Step 7.2 — Filter Depots by Platform

Each depot may have a `config/oslist` key listing valid platforms. Filter to only include depots matching your target OS (`windows`, `linux`, `macos`).

### Step 7.3 — Resolve Manifest ID

For the target branch (default: `public`):
1. Look in `depots/<depotid>/manifests/<branch>` for the manifest ID (uint64)
2. If the branch doesn't exist, fall back to `public`
3. For password-protected branches, call `steamApps.CheckAppBetaPassword(appId, password)` first to get the decryption key, then decrypt the manifest ID from `encryptedmanifests`

### Step 7.4 — Handle depotfromapp

Some depots reference content from another app via `depotfromapp`. In this case:
1. Note the proxy app ID
2. Use it as the `containingAppId` when requesting depot keys and manifest codes
3. The actual content comes from the referenced app's depots

---

## 8. Depot Key: Obtaining Decryption Keys

Every depot's chunks are AES-encrypted. You need the depot-specific decryption key.

### Step 8.1 — Request Depot Key

```protobuf
CMsgClientGetDepotDecryptionKey {
    depot_id: 731
    app_id: 730
}
```

SteamKit2 method: `steamApps.GetDepotDecryptionKey(depotId, appId)`

### Step 8.2 — Receive Depot Key

```protobuf
CMsgClientGetDepotDecryptionKeyResponse {
    eresult: 1           // OK
    depot_id: 731
    depot_encryption_key: <32 bytes>  // AES-256 key
}
```

**Important**: This key is used for both manifest decryption and chunk decryption. Cache it for the duration of the session.

### Step 8.3 — Handle Failures

- `EResult.AccessDenied` → Account doesn't own this depot
- `EResult.Fail` → Invalid depot/app combination

---

## 9. Manifest Request Code: Authorization

Steam requires a time-limited authorization code to download a manifest from CDN. This was added to prevent unauthorized access to old manifest versions.

### Step 9.1 — Request Manifest Code

```protobuf
CContentServerDirectory_GetManifestRequestCode_Request {
    app_id: 730
    depot_id: 731
    manifest_id: 7617088375292372759
    app_branch: "public"
}
```

SteamKit2 method: `steamContent.GetManifestRequestCode(depotId, appId, manifestId, branch)`

### Step 9.2 — Receive Manifest Code

Returns a `uint64` manifest request code. This code:
- Is **time-limited** (typically valid for ~5 minutes)
- Is **specific** to this depot/manifest/branch combination
- Must be passed as a parameter when downloading the manifest from CDN

### Step 9.3 — Handle Code = 0

If the request code is `0`, the manifest cannot be downloaded. This happens when:
- Steam has **blocked** downloading old manifests for this app (developer decision)
- The manifest ID is invalid
- The account lacks access

For anonymous accounts, old manifests are more likely to be blocked. Authenticated accounts may have better luck.

---

## 10. CDN Server Selection

CDN servers are the HTTP endpoints that serve manifest and chunk data.

### Step 10.1 — Get CDN Server List

SteamKit2's `CDNClientPool` manages this. The server list can be obtained from:
- The Steam WebAPI: `IContentServerDirectoryService/GetServersForSteamPipe`
- The CM connection directly

Each server entry contains:
- **Host**: e.g., `cache1-lax1.steamcontent.com`
- **Port**: typically 443 (HTTPS) or 80 (HTTP)
- **Type**: `SteamCache`, `CDN`, `ProxyServer`
- **Cell ID**: geographic affinity
- **Load**: current load metric
- **Weighted Load**: for selection weighting

### Step 10.2 — Server Selection Strategy

DepotDownloader uses **round-robin with penalty weighting**:

1. Maintain a pool of available CDN servers (up to `MaxServers`, default 20)
2. Prefer the same server until it fails (sticky selection)
3. On failure, mark server as broken → apply a penalty score
4. Penalized servers are deprioritized in selection
5. Penalty scores are persisted in `AccountSettingsStore.ContentServerPenalty`

### Step 10.3 — Connection Pooling

Maintain a pool of HTTP connections to CDN servers. When a connection fails:
1. Return it to the pool as "broken"
2. Apply penalty to that server
3. Get a new connection to a different server
4. Retry the failed request

---

## 11. CDN Auth Tokens

Some CDN servers (particularly region-specific ones like those in China) require an additional authentication token.

### Step 11.1 — Request CDN Auth Token

```protobuf
CMsgClientGetCDNAuthToken {
    depot_id: 731
    host_name: "cache1-lax1.steamcontent.com"
}
```

SteamKit2 method: `steamContent.GetCDNAuthToken(depotId, server.Host)`

### Step 11.2 — Receive Token

```protobuf
CMsgClientGetCDNAuthTokenResponse {
    eresult: 1
    token: "..."
    expiration: 1234567890  // Unix timestamp
}
```

### Step 11.3 — Usage

- Pass the token as a query parameter or header when downloading from that specific CDN host
- Tokens are host-specific and time-limited
- Cache tokens and refresh before expiration
- Not all CDN servers require tokens — many work without one

---

## 12. Manifest Download & Parsing

The manifest is the blueprint for all content in a depot. It describes every file, its chunks, and their checksums.

### Step 12.1 — Download Manifest from CDN

HTTP GET request to the CDN server:

```
GET https://<cdn_host>/depot/<depot_id>/manifest/<manifest_id>/5/<manifest_request_code>
```

The `5` in the URL is the manifest version. Headers may include the CDN auth token if required.

### Step 12.2 — Decrypt Manifest

The response body is an encrypted blob:
1. **AES-ECB decrypt** using the depot key from Step 8
2. **Decompress** using zlib (inflate)
3. The result is a binary-serialized manifest structure

### Step 12.3 — Parse Manifest Structure

The decrypted manifest is a custom binary format (not protobuf). It contains:

```
DepotManifest {
    depot_id: uint32
    creation_time: DateTime
    total_uncompressed_size: uint64
    total_compressed_size: uint64
    files: [
        FileData {
            filename: String          // Relative path (e.g., "csgo/maps/de_dust2.bsp")
            size: uint64              // Total uncompressed file size
            flags: EDepotFileFlag     // Executable, Hidden, Directory, etc.
            sha_content: [u8; 20]     // SHA-1 of the entire file content
            chunks: [
                ChunkData {
                    sha: [u8; 20]              // SHA-1 of the uncompressed chunk data
                    checksum: u32              // Adler-32 of the uncompressed chunk data
                    offset: uint64             // Offset within the file where this chunk belongs
                    compressed_length: uint32  // Size of the chunk as stored on CDN
                    uncompressed_length: uint32 // Size after decompression
                }
            ]
        }
    ]
}
```

### Step 12.4 — Cache Manifest Locally

Save the parsed manifest to disk for future delta comparisons:
- Path: `.DepotDownloader/<depot_id>_<manifest_id>.bin`
- This avoids re-downloading the manifest on subsequent runs

---

## 13. Delta Comparison: Old vs New Manifest

If you've previously downloaded this depot, compare manifests to minimize downloads.

### Step 13.1 — Load Previous Manifest

Check `DepotConfigStore.InstalledManifestIDs[depotId]` for the previously installed manifest ID. If found, load the cached manifest from disk.

### Step 13.2 — Compare File Lists

For each file in the **new** manifest:

| Condition | Action |
|---|---|
| File exists in old manifest, same SHA | **Skip entirely** — file is unchanged |
| File exists in old manifest, different SHA | **Partial update** — only download changed chunks |
| File is new (not in old manifest) | **Full download** — download all chunks |

For each file in the **old** manifest but **not** in the new:
- **Delete** the file from disk

### Step 13.3 — Chunk-Level Delta

For files that exist in both manifests but have changed:
1. Build a set of chunk SHAs from the old manifest for this file
2. For each chunk in the new manifest's file entry:
   - If the chunk SHA exists in the old set → chunk is unchanged, skip
   - If the chunk SHA is new → add to download queue

This enables minimal bandwidth usage — only truly changed data is fetched.

### Step 13.4 — Validate Existing Chunks on Disk

Even for "unchanged" chunks, optionally validate what's on disk:
1. Read the existing file at the chunk's offset
2. Compute Adler-32 checksum
3. Compare against the manifest's expected checksum
4. If mismatch → add to download queue

This is triggered by the `-validate` / `-verify-all` flag.

---

## 14. Chunk Download Pipeline

This is the performance-critical path. Each chunk is an independent unit that can be downloaded, decrypted, and written in parallel.

### Step 14.1 — Build Download Queue

Collect all `(depot_id, chunk_data, file_data, file_stream)` tuples for chunks that need downloading.

### Step 14.2 — Pre-allocate Files

For each file with needed chunks:
1. Create parent directories
2. Open (or create) the file in the **staging directory**: `.DepotDownloader/staging/<file_path>`
3. Pre-allocate the file to its final size (avoids fragmentation)
4. Keep the file stream open for concurrent chunk writes

### Step 14.3 — Parallel Chunk Downloads

Launch up to `MaxDownloads` (default 8) concurrent download tasks:

```
for each chunk in download_queue (parallel, bounded by semaphore):
    1. Acquire semaphore slot
    2. Get CDN connection from pool
    3. Download chunk from CDN
    4. Process chunk (decrypt, decompress, verify)
    5. Write to file at correct offset
    6. Release semaphore slot
    7. Update progress counter
```

### Step 14.4 — CDN Chunk Request

HTTP GET to CDN:

```
GET https://<cdn_host>/depot/<depot_id>/chunk/<chunk_sha_hex>
```

The chunk SHA (hex-encoded) serves as the content-addressable key. Headers include CDN auth token if required.

**Response**: Raw bytes — the encrypted, compressed chunk data.

### Step 14.5 — Retry on Failure

If a chunk download fails:
1. Mark the CDN connection as broken (returns to pool with penalty)
2. Do **not** remove the chunk from the queue
3. Acquire a new connection (likely different server)
4. Retry the same chunk
5. After N failures for the same chunk, report error

---

## 15. Chunk Processing: Decrypt, Decompress, Verify

Each downloaded chunk goes through a processing pipeline before being written to disk.

### Step 15.1 — Decrypt

Steam chunks use **AES-256-ECB** encryption with the depot key:

```
plaintext = AES_ECB_Decrypt(depot_key, chunk_bytes)
```

Note: the first bytes of the decrypted blob contain a small header indicating the compression type.

### Step 15.2 — Decompress

After decryption, check the magic bytes to determine compression:

| Magic | Compression | Crate |
|---|---|---|
| `VZ` | **LZMA** (legacy) | `lzma-rs` |
| `0x28 0xB5 0x2F 0xFD` | **Zstd** (newer depots) | `zstd` |
| Other | **Uncompressed** | None |

Decompress to get the raw chunk data. The output size should match `chunk.uncompressed_length`.

### Step 15.3 — Verify Checksum

Compute **Adler-32** over the decompressed chunk data and compare against `chunk.checksum` from the manifest.

If mismatch: the chunk is corrupt. Discard and retry from a different CDN server.

### Step 15.4 — Write to File

Seek to `chunk.offset` in the file stream and write the decompressed data:

```rust
file.seek(SeekFrom::Start(chunk.offset))?;
file.write_all(&decompressed_data)?;
```

Multiple chunks for the same file can be written concurrently as long as they target non-overlapping offsets (which they always do by design).

---

## 16. File Assembly & Finalization

After all chunks for all files are downloaded:

### Step 16.1 — Move from Staging

Atomically move each file from the staging directory to the final install directory:

```
.DepotDownloader/staging/csgo/maps/de_dust2.bsp → <install_dir>/csgo/maps/de_dust2.bsp
```

Use atomic rename where possible to avoid partial files in the install directory.

### Step 16.2 — Set File Permissions

On Linux/macOS, check `file.flags` for `EDepotFileFlag.Executable`:
- If set: `chmod +x` the file
- If previously set but now unset: `chmod -x`

### Step 16.3 — Delete Removed Files

Files present in the old manifest but absent in the new manifest should be deleted from the install directory. Walk the old manifest's file list and remove any files not present in the new manifest.

### Step 16.4 — Update Installed Manifest Record

Persist the newly installed manifest ID:

```
config_store.installed_manifests[depot_id] = new_manifest_id
config_store.save()
```

This enables delta comparison on the next update.

### Step 16.5 — Write Manifest Metadata (Optional)

DepotDownloader optionally writes a human-readable `manifest_<depot_id>_<manifest_id>.txt` listing all files, sizes, chunk counts, and SHAs. Useful for debugging.

---

## 17. Connection Recovery & Error Handling

### CM Connection Recovery

The CM connection can drop due to network issues, server maintenance, or rate limiting.

**Retry strategy** (as implemented by DepotDownloader):
1. On disconnect, check if it was user-initiated or expected (e.g., after Steam Guard challenge)
2. If unexpected: retry up to **10 times** with **linear backoff** (1s × attempt number)
3. Each retry: reset connection state, call `steamClient.Connect()` with a new CM server
4. After 10 failures: abort

### CDN Error Handling

| HTTP Status | Meaning | Action |
|---|---|---|
| **200** | Success | Process chunk |
| **401** | Unauthorized | Refresh CDN auth token, retry |
| **404** | Not Found | Chunk/manifest doesn't exist on this server, try another |
| **500/503** | Server Error | Penalize server, retry with different server |
| **Timeout** | Network issue | Penalize server, retry with different server |

### Token Rejection

If a stored access token is rejected during logon:
- Remove from persistent storage
- Results: `InvalidPassword`, `InvalidSignature`, `AccessDenied`, `Expired`, `Revoked`
- Either abort or fall back to credential-based auth

---

## 18. Concurrency Considerations

### LoginID

Every active CM connection using the same Steam account must have a unique **LoginID** (uint32). If two connections share a LoginID, the newer one kills the older.

For a hosting service running N concurrent updates:
- Auto-generate unique LoginIDs (e.g., hash of process ID + timestamp)
- Or use separate anonymous sessions per update (anonymous sessions are less restrictive)

### CDN Parallelism

- **MaxDownloads** (default 8): Number of concurrent chunk downloads per depot
- **MaxServers** (default 20): Size of the CDN connection pool
- Ensure `MaxServers >= MaxDownloads` so each download task can have its own connection

### File I/O

Multiple chunks for the same file can be written concurrently since they target non-overlapping offsets. However, you need to ensure:
- The file stream supports concurrent seeks/writes (or use `pwrite` on Linux)
- File pre-allocation is done before parallel writes begin

---

## 19. Key Data Structures

### DepotManifest

```
DepotManifest
├── depot_id: u32
├── creation_time: DateTime
├── total_uncompressed_size: u64
├── total_compressed_size: u64
└── files: Vec<FileData>
    ├── filename: String
    ├── size: u64
    ├── flags: EDepotFileFlag
    ├── sha_content: [u8; 20]
    └── chunks: Vec<ChunkData>
        ├── sha: [u8; 20]
        ├── checksum: u32 (Adler-32)
        ├── offset: u64
        ├── compressed_length: u32
        └── uncompressed_length: u32
```

### Key Identifiers

| ID | Type | Description |
|---|---|---|
| **App ID** | u32 | Identifies a game/application (e.g., 730 = CS2) |
| **Depot ID** | u32 | Identifies a content depot within an app |
| **Manifest ID** | u64 | Identifies a specific version/snapshot of a depot |
| **Package ID / Sub ID** | u32 | Identifies a license/subscription granting access to apps/depots |
| **Chunk SHA** | [u8; 20] | Content-addressable ID for a chunk on CDN |
| **CellID** | u32 | Geographic region identifier |
| **LoginID** | u32 | Unique session identifier per CM connection |

### Encryption & Compression

| Operation | Algorithm | Key/Details |
|---|---|---|
| CM connection (TCP) | Custom envelope encryption | Key exchange during handshake |
| CM connection (WebSocket) | TLS | Standard HTTPS |
| Password transmission | RSA | Per-account public key from Steam |
| Manifest decryption | AES-256-ECB | Depot key |
| Chunk decryption | AES-256-ECB | Depot key |
| Manifest compression | zlib (inflate) | After decryption |
| Chunk compression | LZMA or Zstd | After decryption, magic-byte detected |
| Chunk verification | Adler-32 | Against manifest checksum |
| File verification | SHA-1 | Against manifest sha_content |

---

## 20. Relevant Crates for Rust Implementation

| Purpose | Crate | Notes |
|---|---|---|
| **Protobuf** | `prost` + `prost-build` | Compile .proto files from SteamDatabase/Protobufs |
| **WebSocket** | `tokio-tungstenite` | For CM connection |
| **HTTP client** | `reqwest` with `rustls-tls` | For CDN downloads. **Use rustls, not native-tls** for static builds |
| **TLS** | `rustls` | Avoids OpenSSL dependency |
| **Async runtime** | `tokio` | Foundation for concurrent downloads |
| **AES** | `aes` + `ecb` (or `aes` with manual ECB) | For depot key encryption/decryption |
| **LZMA** | `lzma-rs` | For legacy chunk decompression |
| **Zstd** | `zstd` | For newer chunk decompression |
| **Adler-32** | `adler` or `adler32` | For chunk checksum verification |
| **SHA-1** | `sha1` | For chunk SHA and file verification |
| **JSON** | `serde_json` | For CM server list, structured output |
| **CLI** | `clap` | For argument parsing |
| **Build target** | `x86_64-unknown-linux-musl` | For fully static binary |

### Proto Sources

Clone and compile:

```bash
git clone https://github.com/SteamDatabase/Protobufs.git
# Relevant directories:
#   steam/             — Core Steam client messages
#   steam/steammessages_clientserver.proto
#   steam/steammessages_auth.steamclient.proto
#   steam/content_manifest.proto
```

Use `prost-build` in your `build.rs` to generate Rust types from these `.proto` files.

---

## Summary: The Complete Flow

```
1.  Fetch CM server list                  (HTTP GET → WebAPI)
2.  Connect to CM server                  (WebSocket)
3.  Authenticate                          (Protobuf → CM)
4.  Receive license list                  (Protobuf ← CM)
5.  Request app info (PICS)               (Protobuf → CM)
6.  Filter depots by OS/arch              (Local logic)
7.  Resolve manifest ID for branch        (Local logic from app info)
8.  Request depot decryption key          (Protobuf → CM)
9.  Request manifest request code         (Protobuf → CM)
10. Select CDN server                     (From server list)
11. Get CDN auth token (if needed)        (Protobuf → CM)
12. Download manifest from CDN            (HTTPS GET → CDN)
13. Decrypt + decompress manifest         (AES-ECB → zlib)
14. Parse manifest into file/chunk tree   (Custom binary format)
15. Compare against old manifest          (Local diff)
16. Build chunk download queue            (Local logic)
17. Pre-allocate files in staging dir     (Local I/O)
18. Download chunks in parallel           (HTTPS GET → CDN, N concurrent)
19. For each chunk:
    a. Decrypt (AES-ECB)
    b. Decompress (LZMA/Zstd)
    c. Verify (Adler-32)
    d. Write to file at offset
20. Move files from staging → install dir (Atomic rename)
21. Set permissions (chmod +x)            (Linux/macOS)
22. Delete removed files                  (From old manifest diff)
23. Update installed manifest record      (Persist to disk)
24. Disconnect from CM                    (Clean shutdown)
```