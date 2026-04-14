import React from 'react'
import { Link } from 'react-router-dom'

export default function Home() {
  return (
    <div className="min-h-full flex flex-col items-center justify-center px-4 py-16">
      <div className="max-w-md w-full text-center">
        <div className="inline-flex items-center gap-2 text-primary-400 mb-8">
          <svg className="size-8" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
          </svg>
          <span className="font-mono font-bold text-3xl tracking-tight">botster</span>
        </div>
        <p className="text-zinc-400 mb-8">
          Local-first agent platform
        </p>
        <Link
          to="/hubs"
          className="inline-flex items-center gap-2 px-6 py-3 bg-primary-600 hover:bg-primary-500 text-white rounded-lg text-sm font-medium transition-colors"
        >
          Go to Hubs
        </Link>
      </div>
    </div>
  )
}
