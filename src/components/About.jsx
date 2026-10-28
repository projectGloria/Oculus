import { useEffect, useState } from "react";
import { api } from "../api.js";
import { ErrorBanner } from "./ErrorBanner.jsx";

export function About() {
  const [info, setInfo] = useState(null);
  const [error, setError] = useState(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    let alive = true;
    api.getDiagnostics().then((value) => { if (alive) setInfo(value); }).catch((e) => { if (alive) setError(`Could not read diagnostics: ${e}`); });
    return () => { alive = false; };
  }, []);
  return (
    <section className="settings-section" aria-label="About and diagnostics">
      <h2>About</h2>
      {info ? (
        <>
          <p>Oculus {info.version} · Windows {info.windows}</p>
          <p className="muted">Current/last encoder: {info.currentEncoder ?? "Name not available"}</p>
          <details>
            <summary>System diagnostics</summary>
            <pre className="diagnostics">{JSON.stringify(info, null, 2)}</pre>
          </details>
          <button className="btn" onClick={async () => {
            setError(null);
            setCopied(false);
            try { await navigator.clipboard.writeText(JSON.stringify(info, null, 2)); setCopied(true); }
            catch (e) { setError(`Could not copy diagnostics: ${e}`); }
          }}>Copy diagnostics</button>
          {copied && <p role="status">Diagnostics copied.</p>}
        </>
      ) : <p className="muted">Reading system diagnostics…</p>}
      <p className="muted">Recordings stay on this device. No account, uploads or telemetry.</p>
      <ErrorBanner error={error} />
    </section>
  );
}
