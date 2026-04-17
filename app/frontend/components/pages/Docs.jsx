import React from 'react'

export default function Docs() {
  return (
    <div className="min-h-full">
      <div className="max-w-4xl mx-auto px-4 py-8 lg:py-12">
        <h1 className="text-2xl font-bold text-zinc-100 font-mono mb-6">Documentation</h1>
        <div className="prose prose-invert prose-zinc max-w-none">
          <p className="text-zinc-400">
            Documentation is available at{' '}
            <a
              href="/docs"
              className="text-primary-400 hover:text-primary-300"
              target="_blank"
              rel="noopener noreferrer"
            >
              /docs
            </a>
          </p>
        </div>
      </div>
    </div>
  )
}
