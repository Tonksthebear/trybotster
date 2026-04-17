import React from 'react'
import clsx from 'clsx'
import { safeUrl } from '../../lib/actions'

export default function HostedPreviewError({ error, installUrl, density }) {
  if (!error) return null

  const isSidebar = density === 'sidebar'
  const validInstallUrl = safeUrl(installUrl)

  return (
    <div className={isSidebar ? 'px-2 pb-2' : 'px-4 pb-3'}>
      <div
        className={clsx(
          'rounded border bg-red-500/10',
          isSidebar
            ? 'border-red-500/25 px-2 py-1.5'
            : 'flex items-start gap-3 border-red-500/30 rounded-md px-3 py-2'
        )}
      >
        {!isSidebar && (
          <svg
            className="size-4 shrink-0 text-red-300 mt-0.5"
            viewBox="0 0 20 20"
            fill="currentColor"
          >
            <path
              fillRule="evenodd"
              d="M8.485 2.495c.673-1.167 2.357-1.167 3.03 0l6.28 10.875c.673 1.167-.17 2.625-1.516 2.625H3.72c-1.347 0-2.189-1.458-1.515-2.625L8.485 2.495zM10 5a.75.75 0 01.75.75v3.5a.75.75 0 01-1.5 0v-3.5A.75.75 0 0110 5zm0 9a1 1 0 100-2 1 1 0 000 2z"
              clipRule="evenodd"
            />
          </svg>
        )}
        <div className="min-w-0 flex-1">
          <div
            className={clsx(
              'text-red-200',
              isSidebar ? 'text-[10px] leading-4' : 'text-xs leading-5'
            )}
          >
            {error}
          </div>
          {validInstallUrl && (
            <a
              href={validInstallUrl}
              target="_blank"
              rel="noopener noreferrer"
              className={clsx(
                'mt-1 inline-flex font-medium text-red-100 underline decoration-red-300/70 underline-offset-2 hover:text-white hover:decoration-red-100',
                isSidebar ? 'text-[10px]' : 'text-xs'
              )}
            >
              Install cloudflared
            </a>
          )}
        </div>
      </div>
    </div>
  )
}
