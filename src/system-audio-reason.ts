import { i18n } from "./locales";
import type { SystemAudioCapability } from "./types";

// Shared by TranscriptPanel's mode picker and the quick-transcription button,
// so the two never drift on how a capability's machine-readable `reason`
// maps to display text.
export function systemAudioUnavailableLabel(capability: SystemAudioCapability): string {
  const { t } = i18n.global;
  switch (capability.reason) {
    case "unsupported-os":
      return t("transcript.mode.systemUnavailableOs");
    case "unsupported-platform":
      return t("transcript.mode.systemUnavailablePlatform");
    default:
      return t("transcript.mode.systemUnavailable");
  }
}
