// Ambient global surface for the zkv web UI.
//
// The frontend runs as classic scripts (no ES modules): each source file's
// top-level `const Foo` is a real global that other source files reference
// bare (`<Foo>`). TypeScript shares those globals across all script files in
// the program automatically, so components/helpers do NOT need an ambient
// declaration here; porting leaf→root (icon → chrome → … → app) keeps every
// provider in the program before its consumers.
//
// What DOES belong here:
//   * vendored globals not defined in any source file (lucide, Tauri),
//   * the `window.*` surface, since both `window.Foo` reads and `window.Foo =`
//     export assignments need `Window` to carry the property,
//   * the JSON DTOs returned by the backend `Engine` (mirrors of the serde
//     structs in crates/zkv/src/gui/engine.rs; keep the two in sync, field
//     names are snake_case to match the wire format), and the typed `zkvApi`
//     client surface (crates/zkv/src/gui/assets/src/api.ts).
import type * as ReactNS from "react";

declare global {
  // ---- Vendored Lucide icon registry -------------------------------------
  // Icon children are `[tagName, attrs]` tuples. The PascalCase index is
  // `any` so `reg[name]` stays usable without a cast (the registry shape is
  // owned by the vendored lucide.min.js, not us).
  type LucideIconChildren = ReadonlyArray<[string, Record<string, unknown>]>;
  interface LucideRegistry {
    icons?: Record<string, LucideIconChildren>;
    [pascalName: string]: any;
  }

  // ---- Minimal Tauri IPC surface (desktop transport) ---------------------
  interface TauriGlobal {
    core: {
      invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown>;
    };
  }

  // ===================================================================
  // Backend DTOs: mirror crates/zkv/src/gui/engine.rs (snake_case).
  // ===================================================================
  interface StatusResp {
    version: string;
    git_sha: string;
    server: string;
    current: string | null;
    databases: number;
    network: string | null;
    chain_tip: number | null;
    synced: number | null;
    latency_ms: number | null;
    sync_workers: number;
    paused_all: boolean;
    platform: string;
    demo_reimport_available: boolean;
    onboarded: boolean;
    build_out_of_date: boolean;
  }

  interface ServerRow {
    server: string;
    online: boolean;
    block_height: number | null;
    backend: string | null;
  }

  interface ServersResp {
    mainnet: ServerRow;
    testnet: ServerRow;
    data_dir: string;
  }

  interface LicensesResp {
    text: string;
  }

  interface SaveResp {
    saved: boolean;
    path: string | null;
  }

  interface DbSummary {
    name: string;
    role: string;
    network: string;
    pool: string;
    server: string;
    birthday: number;
    keys: number;
    unsynced: number;
    paused: boolean;
    updated_at: number | null;
    synced: number | null;
    /// False for rows from the fast launch list (`listDatabasesBasic`): their
    /// counts are placeholders until the full list fills them in. Always true
    /// from `listDatabases`.
    detailed: boolean;
  }

  interface KeyStatus {
    kind: string; // confirmed | confirming | pending | deleting
    done: number;
    required: number;
  }

  interface KeyRow {
    key: string;
    value: string | null;
    status: KeyStatus;
    txid: string | null;
    deleted: boolean;
    size: number | null;
    updated_at: number | null;
  }

  interface DbDetail {
    name: string;
    role: string;
    network: string;
    pool: string;
    server: string;
    birthday: number;
    address: string;
    funding_address: string | null;
    signer: string | null;
    init: string;
    init_done: number;
    init_required: number;
    balance: number | null;
    confirming: number | null;
    synced: number | null;
    synced_to_tip: boolean;
    keys: KeyRow[];
    history_available: boolean;
    paused: boolean;
    db_version: number;
    client_max_version: number;
    block_flags: string;
    blocks_sync: boolean;
    blocks_read: boolean;
    blocks_write: boolean;
    // Most recent background auto-sync failure for this db (display-ready), or
    // null if the last sync succeeded. Shown as a non-blocking warning banner.
    sync_error?: string | null;
  }

  // The compact per-database projection App builds from a DbDetail for the
  // chrome (Topbar/StatusBar) and the write/deposit modals: a subset of
  // DbDetail fields, but with `keys` as a count rather than the full rows.
  interface ActiveDb {
    name: string;
    role: string;
    network: string;
    pool: string;
    synced: number | null;
    server: string;
    address: string;
    funding_address: string | null;
    signer: string | null;
    balance: number | null;
    confirming: number | null;
    keys: number;
    init: string;
    init_done: number;
    init_required: number;
  }

  interface HistoryStatusResp {
    kind: string; // confirmed | confirming | pending
    done: number;
    required: number;
    confirmations: number;
  }

  interface HistoryEntryResp {
    op: string; // SET | DEL | INIT
    key: string;
    value: string | null;
    height: number | null;
    timestamp: number | null;
    txid: string;
    output_index: number;
    signature: string | null;
    seq: number | null; // replay-protection sequence referenced on the wire
    signer: string | null;
    signer_role: string | null;
    verified: boolean | null;
    status: HistoryStatusResp;
    memo: string | null;
    fee: number | null;
    output_value: number | null; // zatoshi carried by this write's output, if nonzero
  }

  interface HistoryResp {
    creator: string;
    entries: HistoryEntryResp[];
    total: number;
    offset: number;
    limit: number | null;
  }

  interface RoleRow {
    role: string; // owner | writer
    pubkey: string;
    capabilities: string[];
    height: number | null;
    timestamp: number | null;
    granted_by: string | null;
  }

  interface RevokedRoleRow {
    role: string; // owner | writer (held before revocation)
    pubkey: string;
    capabilities: string[];
    height: number | null;
    timestamp: number | null;
    revoked_by: string | null;
  }

  interface RolesResp {
    creator: string | null;
    rows: RoleRow[];
    revoked: RevokedRoleRow[];
  }

  interface RejectionResp {
    op: string | null;
    key: string | null;
    value: string | null;
    height: number | null;
    timestamp: number | null;
    txid: string;
    raw: string;
    reason: string;
    signer: string | null;
    signature_valid: boolean;
  }

  interface RejectionsResp {
    entries: RejectionResp[];
    total: number;
  }

  interface FundingTxResp {
    txid: string;
    height: number | null;
    timestamp: number | null;
    direction: string; // received | sent | self | zkv
    amount: number; // for "self"/"zkv": the net effect (the fee)
    self_sent: number | null; // for "self": gross value routed back to us
    fee: number | null;
    memo: string | null;
    recipients: string[];
    is_zkv: boolean; // carries a zkv memo; detail links to the write in History
    pending: boolean;
    confirmations: number; // on-chain confirmations; 0 while in the mempool
    required: number; // ZIP-315 depth for confirmed: 10 received, 3 sent/self
    confirmed: boolean; // reached `required` confs (matches wallet spendability)
  }

  interface FundingResp {
    entries: FundingTxResp[];
    total: number;
    offset: number;
    limit: number | null;
  }

  interface QrResp {
    svg: string;
  }

  interface SignPreviewResp {
    memo: string;
    recipient: string;
  }

  interface SignMemoResp {
    unsigned: string;
    signed: string;
    recipient_ua: string;
    zkv_addr: string;
  }

  // Outcome of a backend-proxied faucet call. "ok" = accepted; "outdated" =
  // response mentioned "update" OR the faucet was unreachable (so show "Your
  // app is outdated"); "error" = faucet reachable but non-2xx ("Try again
  // later").
  interface FaucetResp {
    outcome: string;
    /// The broadcast txid for the sponsored-INIT path, when the faucet returned
    /// one. Absent for the fund-only call.
    txid?: string | null;
  }

  interface CreateResp {
    name: string;
    address: string;
    phrase: string;
    funding_address: string;
  }

  interface PhraseResp {
    phrase: string;
  }

  interface SyncResp {
    synced: number;
  }
  interface PauseResp {
    paused: boolean;
  }
  interface SettingsResp {
    sync_workers: number;
  }
  interface TxResp {
    txid: string;
  }
  interface AddrCheckResp {
    valid: boolean;
    kind: string | null;
    network: string | null;
    pool: string | null;
    error: string | null;
  }
  interface AddDbResp {
    name: string;
    role: string;
  }
  interface ZkvAddrInfoResp {
    network: string;
    pool: string;
    birthday: number;
  }
  interface OkResp {
    ok: boolean;
  }
  interface RevealPhraseResp {
    name: string;
    phrase: string;
  }

  // ===================================================================
  // zkvApi: the dual-transport client surface (src/api.ts).
  // Method names are camelCase; argument-object keys passed to the backend
  // stay snake_case (require_sync, zkv_address, sync_workers).
  // ===================================================================
  interface HistoryOpts {
    filter?: string;
    limit?: number | null;
    offset?: number;
    locate?: string;
  }
  interface FundingOpts {
    limit?: number | null;
    offset?: number;
  }
  interface InitOpts {
    requireSync?: boolean;
  }

  interface ZkvApi {
    status(): Promise<StatusResp>;
    servers(): Promise<ServersResp>;
    licenses(): Promise<LicensesResp>;
    saveLicenses(): Promise<SaveResp>;
    openUrl(url: string): Promise<void>;
    listDatabases(): Promise<DbSummary[]>;
    listDatabasesBasic(): Promise<DbSummary[]>;
    detail(name: string): Promise<DbDetail>;
    history(name: string, opts?: HistoryOpts): Promise<HistoryResp>;
    rejections(name: string): Promise<RejectionsResp>;
    roles(name: string): Promise<RolesResp>;
    funding(name: string, opts?: FundingOpts): Promise<FundingResp>;
    sync(name: string, mempool?: boolean): Promise<SyncResp>;
    setPause(name: string, paused: boolean): Promise<PauseResp>;
    setSettings(sync_workers: number): Promise<SettingsResp>;
    pauseAll(paused: boolean): Promise<PauseResp>;
    init(name: string, opts?: InitOpts): Promise<TxResp>;
    faucetFunds(name: string): Promise<FaucetResp>;
    faucetInit(name: string): Promise<FaucetResp>;
    set(name: string, key: string, value: string): Promise<TxResp>;
    del(name: string, key: string): Promise<TxResp>;
    send(
      name: string,
      recipient: string,
      amount: string,
      memo?: string | null,
    ): Promise<TxResp>;
    checkAddress(name: string, address: string): Promise<AddrCheckResp>;
    signPreview(
      name: string,
      op: string,
      key: string,
      value: string,
    ): Promise<SignPreviewResp>;
    signMemo(name: string, fields: any): Promise<SignMemoResp>;
    create(
      name: string,
      network: string,
      pool: string,
      phrase?: string | null,
    ): Promise<CreateResp>;
    generatePhrase(): Promise<PhraseResp>;
    openDataDir(): Promise<OkResp>;
    watch(zkv_address: string, name: string | null): Promise<AddDbResp>;
    reimportDemo(): Promise<AddDbResp>;
    markOnboarded(): Promise<unknown>;
    inspectAddress(address: string): Promise<ZkvAddrInfoResp>;
    verifyPhrase(phrase: string, address: string): Promise<boolean>;
    restore(
      name: string,
      phrase: string,
      network: string,
      pool: string,
      birthday?: number,
    ): Promise<AddDbResp>;
    setCurrent(name: string): Promise<OkResp>;
    forget(name: string): Promise<OkResp>;
    revealPhrase(name: string): Promise<RevealPhraseResp>;
    qr(data: string): Promise<QrResp>;
  }

  // A frontend component (props are untyped while the port is loose).
  type ZkvComponent = ReactNS.FC<any>;

  // ===================================================================
  // window.* surface: vendored globals + every cross-file export. Reads
  // (`window.formatZats`) and export assignments (`window.Foo = Foo`) both
  // need these. Components are added to the program as their files are
  // ported; declaring the names here up front is harmless.
  // ===================================================================
  interface Window {
    lucide: LucideRegistry;
    __TAURI__?: TauriGlobal;
    ZKV_TOKEN?: string;
    zkvApi: ZkvApi;

    // shared helpers (chrome)
    formatZats: (
      zats: number | null | undefined,
      network?: string | null,
    ) => string;
    currencyFor: (network?: string | null) => string;
    // Platform helpers (chrome): is this macOS, and the command-modifier label
    // (⌘ on macOS, Ctrl elsewhere) used in keyboard-shortcut hints.
    IS_MAC: boolean;
    MOD_KEY: string;

    // components (one per cross-file export)
    Icon: ZkvComponent;
    PauseGlyph: ZkvComponent;
    Topbar: ZkvComponent;
    Sidebar: ZkvComponent;
    StatusBar: ZkvComponent;
    Settings: ZkvComponent;
    Dashboard: ZkvComponent;
    KeyList: ZkvComponent;
    KeyDetail: ZkvComponent;
    HistoryDetail: ZkvComponent;
    RejectionDetail: ZkvComponent;
    RoleDetail: ZkvComponent;
    FundingDetail: ZkvComponent;
    CollapsibleString: ZkvComponent;
    CopyableBlock: ZkvComponent;
    ErrorMessage: ZkvComponent;
    WriteFlow: ZkvComponent;
    CreateFlow: ZkvComponent;
    ImportFlow: ZkvComponent;
    DepositModal: ZkvComponent;
    SendModal: ZkvComponent;
    Qr: ZkvComponent;
    Discover: ZkvComponent;
    Licenses: ZkvComponent;
    KeyboardShortcuts: ZkvComponent;
    Onboarding: ZkvComponent;
    CommandPalette: ZkvComponent;
    Reference: ZkvComponent;
    // Opcode catalogue published by reference.tsx, consumed by the command
    // palette in discover.tsx.
    ZKV_OPCODES: Array<{ id: string; name: string; kind: string }>;
  }

  // Vendored Lucide global, also reachable bare as `lucide`.
  const lucide: LucideRegistry;
}

export {};
