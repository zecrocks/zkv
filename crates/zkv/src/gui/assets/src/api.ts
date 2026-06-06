// api.ts: dual-transport client for the zkv gui. Same `window.zkvApi`
// surface either way, so the React components don't care which is live:
//
//   * Desktop (`zkv gui`): Tauri is present, so each call is an IPC
//     `invoke('<command>', args)`. Nothing listens on a localhost port.
//   * Browser (`zkv gui-browser`): falls back to `fetch('/api/*')` with the
//     per-launch session token injected into the page as window.ZKV_TOKEN.
(function () {
  const enc = encodeURIComponent;
  const tauri = window.__TAURI__;

  // ---- Tauri IPC transport ------------------------------------------------
  // A command that returns Err(CmdError) makes invoke() reject with the
  // serialized {code, message, available?, required?, pending?}; normalize
  // that into an Error matching the HTTP path's shape (.code/.data/.status).
  async function call(cmd: string, args?: any): Promise<any> {
    try {
      return await tauri!.core.invoke(cmd, args || {});
    } catch (e) {
      const obj: { message?: string; code?: string } =
        e && typeof e === "object" ? e : { message: String(e) };
      const err: any = new Error(obj.message || "request failed");
      err.code = obj.code;
      err.data = obj;
      err.status = undefined; // no HTTP status under IPC
      throw err;
    }
  }

  const ipcApi: ZkvApi = {
    status: () => call("status"),
    servers: () => call("servers"),
    licenses: () => call("licenses"),
    // Native save dialog + external-link opening (desktop only).
    saveLicenses: () => call("save_licenses"),
    openUrl: (url) => call("open_url", { url }),
    listDatabases: () => call("list_databases"),
    detail: (name) => call("detail", { name }),
    history: (name, opts) => {
      const o = opts || {};
      return call("history", {
        name,
        filter: o.filter || null,
        limit: o.limit != null ? o.limit : null,
        offset: o.offset || 0,
        locate: o.locate || null,
      });
    },
    rejections: (name) => call("rejections", { name }),
    roles: (name) => call("roles", { name }),
    funding: (name, opts) => {
      const o = opts || {};
      return call("funding", {
        name,
        limit: o.limit != null ? o.limit : null,
        offset: o.offset || 0,
      });
    },
    sync: (name, mempool) => call("sync", { name, mempool: !!mempool }),
    setPause: (name, paused) => call("set_pause", { name, paused: !!paused }),
    setSettings: (sync_workers) => call("set_settings", { sync_workers }),
    pauseAll: (paused) => call("pause_all", { paused: !!paused }),
    init: (name, opts) =>
      call("init", { name, require_sync: !!(opts && opts.requireSync) }),
    // Backend-proxied faucet calls (see httpApi for the semantics).
    faucetFunds: (name) => call("faucet_funds", { name }),
    faucetInit: (name) => call("faucet_init", { name }),
    set: (name, key, value) => call("set_key", { name, key, value }),
    del: (name, key) => call("del_key", { name, key }),
    // Plain ZEC value transfer to an arbitrary address; `amount` is a decimal
    // ZEC string. `checkAddress` validates a recipient without sending.
    send: (name, recipient, amount, memo) =>
      call("send", { name, recipient, amount, memo: memo ?? null }),
    checkAddress: (name, address) => call("check_address", { name, address }),
    // Sign a write memo without broadcasting (for the live preview). `op` is
    // "set" | "del" | "init"; returns { memo, recipient }.
    signPreview: (name, op, key, value) =>
      call("sign_preview", { name, op, key, value }),
    // Sign a memo without broadcasting (Reference builder). `fields` is
    // { op, key?, value?, scope? }.
    signMemo: (name, fields) => call("sign_memo", { name, ...fields }),
    create: (name, network, pool, phrase) =>
      call("create", { name, network, pool, phrase: phrase ?? null }),
    generatePhrase: () => call("generate_phrase"),
    openDataDir: () => call("open_data_dir"),
    watch: (zkv_address, name) => call("watch", { zkv_address, name }),
    // Re-add the bundled demo database after the user deleted it (Settings).
    reimportDemo: () => call("reimport_demo"),
    // Persist that onboarding has been completed/dismissed (state lives in .zkv).
    markOnboarded: () => call("mark_onboarded"),
    inspectAddress: (address) => call("inspect_address", { address }),
    restore: (name, phrase, network, birthday) =>
      call("restore", { name, phrase, network, birthday }),
    setCurrent: (name) => call("set_current", { name }),
    // Permanently delete a database's local state (the on-chain writes remain).
    forget: (name) => call("forget", { name }),
    // Decrypt and return an admin database's recovery phrase (Danger Zone).
    revealPhrase: (name) => call("reveal_phrase", { name }),
    qr: (data) => call("qr", { data }),
  };

  // ---- HTTP transport (gui-browser) ---------------------------------------
  const token = window.ZKV_TOKEN || "";

  async function req(method: string, path: string, body?: any): Promise<any> {
    const opts: RequestInit & { headers: Record<string, string> } = {
      method,
      headers: { "x-zkv-token": token },
    };
    if (body !== undefined) {
      opts.headers["content-type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    const res = await fetch(path, opts);
    const text = await res.text();
    let data: any = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch (_) {
      /* non-JSON body (shouldn't happen for /api) */
    }
    if (!res.ok) {
      const err: any = new Error((data && data.error) || res.statusText || "request failed");
      err.code = data && data.code;
      err.status = res.status;
      err.data = data;
      throw err;
    }
    return data;
  }

  const httpApi: ZkvApi = {
    status: () => req("GET", "/api/status"),
    servers: () => req("GET", "/api/servers"),
    licenses: () => req("GET", "/api/licenses"),
    // No Tauri here: fetch the bundle and trigger a browser download, and
    // open external links in a new tab. Same `window.zkvApi` surface as the
    // desktop transport so the component doesn't branch on which is live.
    saveLicenses: async () => {
      const r = await req("GET", "/api/licenses");
      const blob = new Blob([(r && r.text) || ""], { type: "text/plain;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "zkv-licenses.txt";
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      return { saved: true, path: null };
    },
    openUrl: (url) => {
      window.open(url, "_blank", "noopener,noreferrer");
      return Promise.resolve();
    },
    listDatabases: () => req("GET", "/api/databases"),
    detail: (name) => req("GET", "/api/databases/" + enc(name)),
    history: (name, opts) => {
      const o = opts || {};
      const p: string[] = [];
      if (o.filter) p.push("filter=" + enc(o.filter));
      if (o.limit != null) p.push("limit=" + o.limit);
      if (o.offset) p.push("offset=" + o.offset);
      if (o.locate) p.push("locate=" + enc(o.locate));
      const qs = p.length ? "?" + p.join("&") : "";
      return req("GET", "/api/databases/" + enc(name) + "/history" + qs);
    },
    rejections: (name) => req("GET", "/api/databases/" + enc(name) + "/rejections"),
    roles: (name) => req("GET", "/api/databases/" + enc(name) + "/roles"),
    funding: (name, opts) => {
      const o = opts || {};
      const p: string[] = [];
      if (o.limit != null) p.push("limit=" + o.limit);
      if (o.offset) p.push("offset=" + o.offset);
      const qs = p.length ? "?" + p.join("&") : "";
      return req("GET", "/api/databases/" + enc(name) + "/funding" + qs);
    },
    sync: (name, mempool) =>
      req("POST", "/api/databases/" + enc(name) + "/sync", { mempool: !!mempool }),
    setPause: (name, paused) =>
      req("POST", "/api/databases/" + enc(name) + "/pause", { paused: !!paused }),
    setSettings: (sync_workers) =>
      req("POST", "/api/settings", { sync_workers }),
    pauseAll: (paused) => req("POST", "/api/pause-all", { paused: !!paused }),
    // `opts.requireSync` gates the broadcast on a full sync to chain tip
    // (used when re-broadcasting INIT on an existing, already-synced db).
    init: (name, opts) =>
      req("POST", "/api/databases/" + enc(name) + "/init", {
        require_sync: !!(opts && opts.requireSync),
      }),
    // Ask the hosted testnet faucet (proxied through the Rust backend, so no
    // browser CORS and errors land under RUST_LOG) to fund this db's address
    // or broadcast a sponsored INIT. Both return { outcome }: "ok" |
    // "outdated" (mentions "update" OR faucet unreachable) | "error" (faucet
    // up but non-2xx).
    faucetFunds: (name) => req("POST", "/api/databases/" + enc(name) + "/faucet"),
    faucetInit: (name) => req("POST", "/api/databases/" + enc(name) + "/faucet-init"),
    set: (name, key, value) =>
      req("POST", "/api/databases/" + enc(name) + "/keys", { key, value }),
    del: (name, key) =>
      req("DELETE", "/api/databases/" + enc(name) + "/keys/" + enc(key)),
    // Plain ZEC value transfer to an arbitrary address; `amount` is a decimal
    // ZEC string. `checkAddress` validates a recipient without sending.
    send: (name, recipient, amount, memo) =>
      req("POST", "/api/databases/" + enc(name) + "/send", { recipient, amount, memo: memo ?? null }),
    checkAddress: (name, address) =>
      req("POST", "/api/databases/" + enc(name) + "/check-address", { address }),
    // Sign a write memo without broadcasting (for the live preview). `op` is
    // "set" | "del" | "init"; returns { memo, recipient }.
    signPreview: (name, op, key, value) =>
      req("POST", "/api/databases/" + enc(name) + "/sign-preview", { op, key, value }),
    // Sign a memo without broadcasting (Reference builder). `fields` is
    // { op, key?, value?, scope? }.
    signMemo: (name, fields) =>
      req("POST", "/api/databases/" + enc(name) + "/sign", fields),
    create: (name, network, pool, phrase) =>
      req("POST", "/api/databases", { name, network, pool, phrase: phrase ?? null }),
    generatePhrase: () => req("POST", "/api/phrase"),
    openDataDir: () => req("POST", "/api/open-data-dir"),
    watch: (zkv_address, name) => req("POST", "/api/watch", { zkv_address, name }),
    // Re-add the bundled demo database after the user deleted it (Settings).
    reimportDemo: () => req("POST", "/api/reimport-demo"),
    // Persist that onboarding has been completed/dismissed (state lives in .zkv).
    markOnboarded: () => req("POST", "/api/onboarded"),
    inspectAddress: (address) => req("POST", "/api/inspect-address", { address }),
    restore: (name, phrase, network, birthday) =>
      req("POST", "/api/restore", { name, phrase, network, birthday }),
    setCurrent: (name) => req("POST", "/api/current", { name }),
    // Permanently delete a database's local state (the on-chain writes remain).
    forget: (name) => req("DELETE", "/api/databases/" + enc(name)),
    // Decrypt and return an admin database's recovery phrase (Danger Zone).
    // POST so the secret never lands in a URL or the browser history.
    revealPhrase: (name) => req("POST", "/api/databases/" + enc(name) + "/reveal-phrase"),
    qr: (data) => req("GET", "/api/qr?data=" + enc(data)),
  };

  window.zkvApi = tauri ? ipcApi : httpApi;
})();
