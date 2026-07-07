(function() {
  const enc = encodeURIComponent;
  const tauri = window.__TAURI__;
  async function call(cmd, args) {
    try {
      return await tauri.core.invoke(cmd, args || {});
    } catch (e) {
      const obj = e && typeof e === "object" ? e : { message: String(e) };
      const err = new Error(obj.message || "request failed");
      err.code = obj.code;
      err.data = obj;
      err.status = void 0;
      throw err;
    }
  }
  const ipcApi = {
    status: () => call("status"),
    servers: () => call("servers"),
    licenses: () => call("licenses"),
    // Native save dialog + external-link opening (desktop only).
    saveLicenses: () => call("save_licenses"),
    openUrl: (url) => call("open_url", { url }),
    listDatabases: () => call("list_databases"),
    listDatabasesBasic: () => call("list_databases_basic"),
    detail: (name) => call("detail", { name }),
    history: (name, opts) => {
      const o = opts || {};
      return call("history", {
        name,
        filter: o.filter || null,
        limit: o.limit != null ? o.limit : null,
        offset: o.offset || 0,
        locate: o.locate || null
      });
    },
    rejections: (name) => call("rejections", { name }),
    roles: (name) => call("roles", { name }),
    funding: (name, opts) => {
      const o = opts || {};
      return call("funding", {
        name,
        limit: o.limit != null ? o.limit : null,
        offset: o.offset || 0
      });
    },
    sync: (name, mempool) => call("sync", { name, mempool: !!mempool }),
    setPause: (name, paused) => call("set_pause", { name, paused: !!paused }),
    setSettings: (sync_workers) => call("set_settings", { sync_workers }),
    pauseAll: (paused) => call("pause_all", { paused: !!paused }),
    init: (name, opts) => call("init", { name, require_sync: !!(opts && opts.requireSync) }),
    // Backend-proxied faucet calls (see httpApi for the semantics).
    faucetFunds: (name) => call("faucet_funds", { name }),
    faucetInit: (name) => call("faucet_init", { name }),
    set: (name, key, value) => call("set_key", { name, key, value }),
    del: (name, key) => call("del_key", { name, key }),
    // Plain ZEC value transfer to an arbitrary address; `amount` is a decimal
    // ZEC string. `checkAddress` validates a recipient without sending.
    send: (name, recipient, amount, memo) => call("send", { name, recipient, amount, memo: memo ?? null }),
    checkAddress: (name, address) => call("check_address", { name, address }),
    // Sign a write memo without broadcasting (for the live preview). `op` is
    // "set" | "del" | "init"; returns { memo, recipient }.
    signPreview: (name, op, key, value) => call("sign_preview", { name, op, key, value }),
    // Sign a memo without broadcasting (Reference builder). `fields` is
    // { op, key?, value?, scope? }.
    signMemo: (name, fields) => call("sign_memo", { name, ...fields }),
    create: (name, network, pool, phrase) => call("create", { name, network, pool, phrase: phrase ?? null }),
    generatePhrase: () => call("generate_phrase"),
    openDataDir: () => call("open_data_dir"),
    watch: (zkv_address, name) => call("watch", { zkv_address, name }),
    // Re-add the bundled demo database after the user deleted it (Settings).
    reimportDemo: () => call("reimport_demo"),
    // Persist that onboarding has been completed/dismissed (state lives in .zkv).
    markOnboarded: () => call("mark_onboarded"),
    inspectAddress: (address) => call("inspect_address", { address }),
    verifyPhrase: (phrase, address) => call("verify_phrase", { phrase, address }),
    restore: (name, phrase, network, pool, birthday) => call("restore", { name, phrase, network, pool, birthday }),
    setCurrent: (name) => call("set_current", { name }),
    // Permanently delete a database's local state (the on-chain writes remain).
    forget: (name) => call("forget", { name }),
    // Decrypt and return an admin database's recovery phrase (Danger Zone).
    revealPhrase: (name) => call("reveal_phrase", { name }),
    qr: (data) => call("qr", { data })
  };
  const token = window.ZKV_TOKEN || "";
  async function req(method, path, body) {
    const opts = {
      method,
      headers: { "x-zkv-token": token }
    };
    if (body !== void 0) {
      opts.headers["content-type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    const res = await fetch(path, opts);
    const text = await res.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch (_) {
    }
    if (!res.ok) {
      const err = new Error(data && data.error || res.statusText || "request failed");
      err.code = data && data.code;
      err.status = res.status;
      err.data = data;
      throw err;
    }
    return data;
  }
  const httpApi = {
    status: () => req("GET", "/api/status"),
    servers: () => req("GET", "/api/servers"),
    licenses: () => req("GET", "/api/licenses"),
    // No Tauri here: fetch the bundle and trigger a browser download, and
    // open external links in a new tab. Same `window.zkvApi` surface as the
    // desktop transport so the component doesn't branch on which is live.
    saveLicenses: async () => {
      const r = await req("GET", "/api/licenses");
      const blob = new Blob([r && r.text || ""], { type: "text/plain;charset=utf-8" });
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
    listDatabasesBasic: () => req("GET", "/api/databases?basic=true"),
    detail: (name) => req("GET", "/api/databases/" + enc(name)),
    history: (name, opts) => {
      const o = opts || {};
      const p = [];
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
      const p = [];
      if (o.limit != null) p.push("limit=" + o.limit);
      if (o.offset) p.push("offset=" + o.offset);
      const qs = p.length ? "?" + p.join("&") : "";
      return req("GET", "/api/databases/" + enc(name) + "/funding" + qs);
    },
    sync: (name, mempool) => req("POST", "/api/databases/" + enc(name) + "/sync", { mempool: !!mempool }),
    setPause: (name, paused) => req("POST", "/api/databases/" + enc(name) + "/pause", { paused: !!paused }),
    setSettings: (sync_workers) => req("POST", "/api/settings", { sync_workers }),
    pauseAll: (paused) => req("POST", "/api/pause-all", { paused: !!paused }),
    // `opts.requireSync` gates the broadcast on a full sync to chain tip
    // (used when re-broadcasting INIT on an existing, already-synced db).
    init: (name, opts) => req("POST", "/api/databases/" + enc(name) + "/init", {
      require_sync: !!(opts && opts.requireSync)
    }),
    // Ask the hosted testnet faucet (proxied through the Rust backend, so no
    // browser CORS and errors land under RUST_LOG) to fund this db's address
    // or broadcast a sponsored INIT. Both return { outcome }: "ok" |
    // "outdated" (mentions "update" OR faucet unreachable) | "error" (faucet
    // up but non-2xx).
    faucetFunds: (name) => req("POST", "/api/databases/" + enc(name) + "/faucet"),
    faucetInit: (name) => req("POST", "/api/databases/" + enc(name) + "/faucet-init"),
    set: (name, key, value) => req("POST", "/api/databases/" + enc(name) + "/keys", { key, value }),
    del: (name, key) => req("DELETE", "/api/databases/" + enc(name) + "/keys/" + enc(key)),
    // Plain ZEC value transfer to an arbitrary address; `amount` is a decimal
    // ZEC string. `checkAddress` validates a recipient without sending.
    send: (name, recipient, amount, memo) => req("POST", "/api/databases/" + enc(name) + "/send", { recipient, amount, memo: memo ?? null }),
    checkAddress: (name, address) => req("POST", "/api/databases/" + enc(name) + "/check-address", { address }),
    // Sign a write memo without broadcasting (for the live preview). `op` is
    // "set" | "del" | "init"; returns { memo, recipient }.
    signPreview: (name, op, key, value) => req("POST", "/api/databases/" + enc(name) + "/sign-preview", { op, key, value }),
    // Sign a memo without broadcasting (Reference builder). `fields` is
    // { op, key?, value?, scope? }.
    signMemo: (name, fields) => req("POST", "/api/databases/" + enc(name) + "/sign", fields),
    create: (name, network, pool, phrase) => req("POST", "/api/databases", { name, network, pool, phrase: phrase ?? null }),
    generatePhrase: () => req("POST", "/api/phrase"),
    openDataDir: () => req("POST", "/api/open-data-dir"),
    watch: (zkv_address, name) => req("POST", "/api/watch", { zkv_address, name }),
    // Re-add the bundled demo database after the user deleted it (Settings).
    reimportDemo: () => req("POST", "/api/reimport-demo"),
    // Persist that onboarding has been completed/dismissed (state lives in .zkv).
    markOnboarded: () => req("POST", "/api/onboarded"),
    inspectAddress: (address) => req("POST", "/api/inspect-address", { address }),
    verifyPhrase: (phrase, address) => req("POST", "/api/verify-phrase", { phrase, address }),
    restore: (name, phrase, network, pool, birthday) => req("POST", "/api/restore", { name, phrase, network, pool, birthday }),
    setCurrent: (name) => req("POST", "/api/current", { name }),
    // Permanently delete a database's local state (the on-chain writes remain).
    forget: (name) => req("DELETE", "/api/databases/" + enc(name)),
    // Decrypt and return an admin database's recovery phrase (Danger Zone).
    // POST so the secret never lands in a URL or the browser history.
    revealPhrase: (name) => req("POST", "/api/databases/" + enc(name) + "/reveal-phrase"),
    qr: (data) => req("GET", "/api/qr?data=" + enc(data))
  };
  window.zkvApi = tauri ? ipcApi : httpApi;
})();
