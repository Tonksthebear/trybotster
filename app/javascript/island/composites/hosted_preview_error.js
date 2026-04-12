import { html } from "htm/preact";
import { safeUrl } from "island/actions";

// HostedPreviewErrorPropsV1:
//   error: string | null
//   installUrl: string | null

export function HostedPreviewError({ error, installUrl }) {
  if (!error) return null;

  const safeInstallUrl = safeUrl(installUrl);

  return html`
    <div class="mx-3 mt-1 mb-2 px-3 py-2 rounded-md bg-red-950/50 border border-red-900/50">
      <div class="flex items-start gap-2">
        <span class="text-red-400 text-xs mt-0.5 shrink-0">!</span>
        <div class="min-w-0 flex-1">
          <p class="text-xs text-red-300 break-words">${error}</p>
          ${safeInstallUrl &&
          html`
            <a
              href=${safeInstallUrl}
              target="_blank"
              rel="noopener"
              class="inline-block mt-1.5 text-xs text-red-400 hover:text-red-300 underline underline-offset-2"
            >
              Install Cloudflare tunnel
            </a>
          `}
        </div>
      </div>
    </div>
  `;
}
