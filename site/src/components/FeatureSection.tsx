import { motion, useInView } from 'framer-motion'
import { useRef } from 'react'

interface Bullet {
  icon?: React.ReactNode
  text: string
}

interface FeatureSectionProps {
  tag: string
  tagColor?: string
  heading: string
  body: React.ReactNode
  bullets: Bullet[]
  imageSrc: string
  imageAlt: string
  imageWidth: number
  imageHeight: number
  reverse?: boolean
  accentColor?: string
  children?: React.ReactNode
}

export default function FeatureSection({
  tag,
  tagColor = '#5b8dee',
  heading,
  body,
  bullets,
  imageSrc,
  imageAlt,
  imageWidth,
  imageHeight,
  reverse = false,
  accentColor = '#5b8dee',
  children,
}: FeatureSectionProps) {
  const ref = useRef<HTMLDivElement>(null)
  const inView = useInView(ref, { once: true, margin: '-80px' })

  const textCol = (
    <motion.div
      initial={{ opacity: 0, x: reverse ? 24 : -24 }}
      animate={inView ? { opacity: 1, x: 0 } : {}}
      transition={{ duration: 0.6, ease: [0.22, 1, 0.36, 1] }}
      className="flex flex-col justify-center"
    >
      <span
        className="inline-flex items-center gap-1.5 text-xs font-mono uppercase tracking-widest mb-5 w-fit px-3 py-1 rounded-full"
        style={{
          color: tagColor,
          background: `${tagColor}14`,
          border: `1px solid ${tagColor}28`,
        }}
      >
        {tag}
      </span>
      <h2 className="section-heading mb-5">{heading}</h2>
      <div className="section-subheading mb-8">{body}</div>
      <ul className="space-y-3">
        {bullets.map((b, i) => (
          <li key={i} className="flex items-start gap-3">
            <span
              className="mt-0.5 flex-shrink-0 w-5 h-5 rounded flex items-center justify-center"
              style={{ background: `${accentColor}18`, color: accentColor }}
            >
              {b.icon ?? <CheckIcon />}
            </span>
            <span className="text-sm text-text-secondary leading-relaxed">{b.text}</span>
          </li>
        ))}
      </ul>
      {children && <div className="mt-8">{children}</div>}
    </motion.div>
  )

  const imageCol = (
    <motion.div
      initial={{ opacity: 0, x: reverse ? -24 : 24 }}
      animate={inView ? { opacity: 1, x: 0 } : {}}
      transition={{ duration: 0.65, delay: 0.05, ease: [0.22, 1, 0.36, 1] }}
      className="flex items-center justify-center"
    >
      <div className="relative w-full max-w-xl rounded-xl overflow-hidden border border-bg-border shadow-[0_0_60px_rgba(0,0,0,0.5)]"
           style={{ boxShadow: `0 0 60px rgba(0,0,0,0.5), 0 0 40px ${accentColor}14` }}>
        <img
          src={imageSrc}
          alt={imageAlt}
          width={imageWidth}
          height={imageHeight}
          loading="lazy"
          className="w-full h-auto block"
          style={{ background: '#0f0f17' }}
        />
      </div>
    </motion.div>
  )

  return (
    <section ref={ref} className="py-20 lg:py-28">
      <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
        <div className={`grid lg:grid-cols-2 gap-12 lg:gap-20 items-center ${reverse ? 'lg:[&>*:first-child]:order-last' : ''}`}>
          {reverse ? imageCol : textCol}
          {reverse ? textCol : imageCol}
        </div>
      </div>
    </section>
  )
}

function CheckIcon() {
  return (
    <svg className="w-3 h-3" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
    </svg>
  )
}
