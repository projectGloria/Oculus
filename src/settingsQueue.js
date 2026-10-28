/** Serialize full-snapshot writes while preserving the latest optimistic user intent. */
export function createSettingsQueue(save, onChange, onError) {
  let optimistic = null;
  let confirmed = null;
  let revision = 0;
  let tail = Promise.resolve();

  return {
    hydrate(settings) {
      if (revision !== 0) return;
      optimistic = confirmed = settings;
      onChange(settings);
    },
    update(patch) {
      if (!optimistic) return Promise.resolve();
      const next = { ...optimistic, ...patch };
      optimistic = next;
      const request = ++revision;
      onChange(next);
      onError(null);
      tail = tail.then(async () => {
        try {
          const saved = await save(next);
          confirmed = saved;
          if (request === revision) {
            optimistic = saved;
            onChange(saved);
          }
        } catch (error) {
          if (request === revision) {
            optimistic = confirmed;
            onChange(confirmed);
            onError(String(error));
          }
        }
      });
      return tail;
    },
  };
}
