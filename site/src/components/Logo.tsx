interface LogoProps {
  size?: 'sm' | 'md' | 'lg'
  className?: string
}

const sizeClasses = {
  sm: 'text-xl',
  md: 'text-2xl',
  lg: 'text-3xl',
}

export default function Logo({ size = 'md', className = '' }: LogoProps) {
  return (
    <span
      className={`font-bold tracking-tight select-none ${sizeClasses[size]} ${className}`}
      style={{ letterSpacing: '-0.5px' }}
    >
      <span style={{ color: '#e8e8e8' }}>sven</span>
      <span style={{ color: '#5b8dee' }}>.</span>
    </span>
  )
}
