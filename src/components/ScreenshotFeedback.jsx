import { useEffect, useState } from "react";
import { api, fileName } from "../api.js";
import { useRecorder } from "../hooks/useRecorder.js";
import { Camera, Close } from "./Icons.jsx";
import { useFileDrag } from "../hooks/useFileDrag.js";

export function ScreenshotFeedback() {
  const rec = useRecorder();
  const output = rec.lastScreenshot;
  return output ? <Preview key={output.path} output={output} /> : null;
}

function Preview({ output }) {
  const [thumbnail, setThumbnail] = useState(null);
  const [error, setError] = useState(null);
  const drag = useFileDrag(output.path, setError);
  useEffect(() => {
    let alive = true;
    api.savedFileDetails(output.path).then((d) => { if (alive) setThumbnail(d.thumbnail); }).catch(() => {});
    return () => { alive = false; };
  }, [output.path]);
  return (
    <section className="shot-feedback" aria-label="Screenshot saved">
      <div className="shot-heading">
        <p role="status">Screenshot saved</p>
        <button className="icon-btn" aria-label="Dismiss screenshot preview" onClick={() => api.dismissScreenshotFeedback().catch((e) => setError(String(e)))}><Close /></button>
      </div>
      <button className="shot-preview" {...drag} aria-label="Open screenshot" title="Click to open, or drag into another app" onClick={() => api.openSavedFile(output.path).catch((e) => setError(`Could not open screenshot: ${e}`))}>
        {thumbnail ? <img src={thumbnail} alt="" draggable={false} /> : <Camera />}
      </button>
      <p className="ellipsis" title={output.path}>{fileName(output.path)}</p>
      {error && <p className="shot-error" role="alert">{error}</p>}
    </section>
  );
}
