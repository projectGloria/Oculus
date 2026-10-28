/** Coalesce focus events, while explicit refreshes remain immediate. */
export function createSourceRefresh(load, onSources, onError) {
  let timer;
  let revision = 0;
  let disposed = false;

  const refresh = async () => {
    clearTimeout(timer);
    if (disposed) return;
    const request = ++revision;
    onError(null);
    try {
      const sources = await load();
      if (!disposed && request === revision) onSources(sources);
    } catch (error) {
      if (!disposed && request === revision) onError(`Could not refresh capture sources: ${error}`);
    }
  };

  return {
    refresh,
    focus() {
      if (disposed) return;
      clearTimeout(timer);
      timer = setTimeout(refresh, 200);
    },
    dispose() {
      disposed = true;
      clearTimeout(timer);
    },
  };
}
