import { useState } from "react";

/** Key each occurrence by its message; a cleared error unmounts the dismissed banner. */
export function ErrorBanner({ error }) {
  return error ? <Banner key={String(error)} error={String(error)} /> : null;
}

function Banner({ error }) {
  const [dismissed, setDismissed] = useState(false);
  const [copyStatus, setCopyStatus] = useState("");
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(error);
      setCopyStatus("Details copied.");
    } catch {
      setCopyStatus("Could not copy. Select the details above and press Ctrl+C.");
    }
  };

  if (dismissed) return null;
  return (
    <section className="error-banner" aria-label="Error">
      <p className="error-details" role="alert" tabIndex={0}>{error}</p>
      <div className="error-actions">
        <button className="link" onClick={copy}>Copy details</button>
        <button className="link" onClick={() => setDismissed(true)}>Dismiss</button>
      </div>
      {copyStatus && <p className="error-copy-status" role="status">{copyStatus}</p>}
    </section>
  );
}
