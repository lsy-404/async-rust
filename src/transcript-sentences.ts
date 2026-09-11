// Splits raw transcript text into sentences for the reference's segmented
// display. Works on plain text only: no word timings are involved.
const SENTENCE_PATTERN = /[^。！？.!?\n]+[。！？.!?]?|\n+/g;

export function splitTranscriptSentences(text: string): string[] {
  const trimmed = text.trim();
  if (!trimmed) return [];
  const parts = trimmed.match(SENTENCE_PATTERN) ?? [trimmed];
  return parts
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
}
