import Header from './components/Header'
import Hero from './components/Hero'
import StatsBar from './components/StatsBar'
import FeatureGrid from './components/FeatureGrid'
import { TUIFeature, GDBFeature, CIFeature } from './components/DeepFeatures'
import InstallSection from './components/InstallSection'
import Footer from './components/Footer'
import FeaturePage from './components/FeaturePage'
import { pageFor } from './pages'

/// The page at `path`: one of the feature pages (src/pages.ts), or home.
export default function App({ path = '/' }: { path?: string }) {
  const page = pageFor(path)
  if (page) {
    return (
      <div className="min-h-screen bg-bg-base">
        <Header />
        <FeaturePage page={page} />
        <Footer />
      </div>
    )
  }
  return (
    <div className="min-h-screen bg-bg-base">
      <Header />
      <main>
        <Hero />
        <StatsBar />
        <FeatureGrid />

        {/* Divider */}
        <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
          <div className="border-t border-bg-border" />
        </div>

        <TUIFeature />

        <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
          <div className="border-t border-bg-border" />
        </div>

        <GDBFeature />

        <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
          <div className="border-t border-bg-border" />
        </div>

        <CIFeature />

        <InstallSection />
      </main>
      <Footer />
    </div>
  )
}
