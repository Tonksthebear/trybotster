import { describe, it, expect } from 'vitest'
import { render, screen } from '@testing-library/react'
import Home from '../components/pages/Home'

describe('Home', () => {
  it('renders the GitHub sign-in call to action', () => {
    render(<Home />)

    expect(screen.getByText('botster')).toBeInTheDocument()

    const signInLink = screen.getByRole('link', { name: /sign in with github/i })
    expect(signInLink).toHaveAttribute('href', '/github/authorization/new')
  })
})
