export function encodeMonoWav(
  chunks: Float32Array[],
  sampleRate: number,
): Uint8Array {
  const length = chunks.reduce((total, chunk) => total + chunk.length, 0),
    buffer = new ArrayBuffer(44 + length * 2),
    view = new DataView(buffer);
  const put = (at: number, text: string) =>
    [...text].forEach((c, i) => view.setUint8(at + i, c.charCodeAt(0)));
  put(0, "RIFF");
  view.setUint32(4, 36 + length * 2, true);
  put(8, "WAVEfmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  put(36, "data");
  view.setUint32(40, length * 2, true);
  let at = 44;
  for (const chunk of chunks)
    for (const sample of chunk) {
      view.setInt16(
        at,
        Math.round(Math.max(-1, Math.min(1, sample)) * 32767),
        true,
      );
      at += 2;
    }
  return new Uint8Array(buffer);
}
