// dashboard.jsx: Workspace landing. A live overview of the local
// databases (counts + a card per database). Pinned-key cards and the
// ZEC/USD ticker from the prototype need a price feed / pin store that
// zkv doesn't have yet, so this shows the real workspace instead.

const Dashboard = ({ databases, onOpenDb, onCreate, booted }: {
  databases: DbSummary[];
  detail?: DbDetail | null;
  onOpenDb: (name: string) => void;
  onCreate: () => void;
  booted: boolean;
}) => {
  const totalKeys = databases.reduce((s, d) => s + (d.keys || 0), 0);
  const adminCount = databases.filter((d) => d.role === "admin").length;
  const watchCount = databases.filter((d) => d.role === "watch").length;

  return (
    <div className="dash">
      <div className="dash-header">
        <div>
          <div style={{ fontFamily: "IBM Plex Mono", fontSize: 11, letterSpacing: "0.12em", textTransform: "uppercase", color: "var(--fg-3)" }}>
            WORKSPACE
          </div>
          <h2 style={{ fontFamily: "IBM Plex Sans", fontWeight: 600, fontSize: 26, letterSpacing: "-0.01em", margin: "4px 0 0", color: "var(--fg-1)" }}>
            Dashboard
          </h2>
        </div>
        <div className="dash-stats">
          <div>
            <div className="dash-stat-label">DATABASES</div>
            <div className="dash-stat-value">{databases.length}</div>
          </div>
          <div>
            <div className="dash-stat-label">KEYS TRACKED</div>
            <div className="dash-stat-value">{totalKeys}</div>
          </div>
          <div>
            <div className="dash-stat-label">OWNER/WRITER</div>
            <div className="dash-stat-value">{adminCount}</div>
          </div>
          <div>
            <div className="dash-stat-label">VIEWER</div>
            <div className="dash-stat-value">{watchCount}</div>
          </div>
        </div>
      </div>

      {booted && databases.length === 0 ? (
        <div className="empty" style={{ padding: "72px 24px" }}>
          <div className="glyph"><Icon name="database" size={28} /></div>
          <div style={{ maxWidth: 420, margin: "0 auto", lineHeight: 1.55 }}>
            No databases yet. Create your first one, or watch a public <code style={{ fontFamily: "var(--font-mono)" }}>zkv1…</code> address.
            <div style={{ marginTop: 16 }}>
              <button className="btn primary sm" onClick={onCreate}>
                <Icon name="plus" className="icon" /> Create a database
              </button>
            </div>
          </div>
        </div>
      ) : (
        <div className="dash-grid">
          {databases.map((d) => (
            <div key={d.name} className="dash-card span-medium" style={{ cursor: "pointer" }} onClick={() => onOpenDb(d.name)}>
              <div className="dash-card-head">
                <div className="dash-source">
                  <Icon name={d.role === "admin" ? "database" : "eye"} size={11} color="var(--fg-3)" />
                  <span>{d.name}</span>
                </div>
                <div className="dash-card-actions">
                  <button className="btn ghost sm" title="Open" onClick={(e) => { e.stopPropagation(); onOpenDb(d.name); }}>
                    <Icon name="arrow-up-right" className="icon" />
                  </button>
                </div>
              </div>
              <div className="dash-card-body" style={{ flex: 1, display: "flex", flexDirection: "column" }}>
                <div className="dash-label">{d.role === "admin" ? "Admin · writable" : "Watching · read-only"}</div>
                <div className="dash-value">{d.keys}</div>
                <div className="dash-unit">{d.keys === 1 ? "key" : "keys"}{d.unsynced > 0 ? ` · ${d.unsynced} in flight` : ""}</div>
              </div>
              <div className="dash-card-foot">
                <span><Icon name="git-branch" size={10} /> {d.network}{d.pool ? ` · ${d.pool}` : ""}</span>
                <span>{d.updated_at != null ? `updated ${fmtAgo(d.updated_at)}` : "no writes yet"}</span>
              </div>
            </div>
          ))}
          <button className="dash-add" onClick={onCreate}>
            <Icon name="plus" size={18} />
            <span>Create a new database</span>
          </button>
        </div>
      )}
    </div>
  );
};

window.Dashboard = Dashboard;
