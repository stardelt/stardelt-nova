export default function Overview() {
  return (
    <div className="max-w-3xl space-y-6">
      <h1 className="text-3xl font-bold tracking-tight">Welcome</h1>
      <p className="text-text-dim">
        Stardelt Nova MVP. Use the left nav to browse the catalog, run SQL against Trino,
        and inspect the cluster.
      </p>
      <div className="grid grid-cols-2 gap-4">
        <Card title="Storage"       body="SeaweedFS" />
        <Card title="Catalog"       body="Lakekeeper" />
        <Card title="SQL engine"    body="Trino" />
        <Card title="Orchestration" body="Airflow" />
        <Card title="BI / Display"  body="Superset" />
        <Card title="UI"            body="Nova" />
      </div>
    </div>
  )
}

function Card({ title, body }: { title: string; body: string }) {
  return (
    <div className="rounded-[14px] border border-border bg-bg-elev/60 p-5">
      <div className="text-xs uppercase tracking-widest text-text-mute mb-2">{title}</div>
      <div className="text-lg">{body}</div>
    </div>
  )
}
