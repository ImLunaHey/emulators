// One subsystem row in the hardware-support table: a key label + its prose.
export function SpecRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="spec-row">
      <div className="spec-key">{label}</div>
      <div>{value}</div>
    </div>
  );
}
