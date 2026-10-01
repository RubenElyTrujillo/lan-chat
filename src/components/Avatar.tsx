const PASTELS = ["#E9E3F8", "#F6EAD3", "#DCEEE0", "#F9E3DD", "#DCE9F6", "#EFE9D8"];

function initials(name: string): string {
  const words = name.trim().split(/\s+/);
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  const first = words[0][0];
  const last = words[words.length - 1][0];
  return (first + last).toUpperCase();
}

function tone(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) | 0;
  return PASTELS[Math.abs(hash) % PASTELS.length];
}

export function Avatar({
  name,
  online,
  size = 40,
}: {
  name: string;
  online?: boolean;
  size?: number;
}) {
  return (
    <span className="avatar" style={{ width: size, height: size, background: tone(name) }}>
      {initials(name)}
      {online !== undefined && (
        <span className={`avatar-dot ${online ? "is-online" : "is-offline"}`} />
      )}
    </span>
  );
}
