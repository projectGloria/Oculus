export const gainToDb = (gain) => gain > 0 ? Math.max(-59.5, Math.min(6, 20 * Math.log10(gain))) : -60;
export const dbToGain = (db) => db <= -60 ? 0 : 10 ** (db / 20);
export const formatGain = (gain) => gain > 0 ? `${gain > 1 ? "+" : ""}${(20 * Math.log10(gain)).toFixed(1)} dB` : "−∞ dB";
