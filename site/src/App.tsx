import Header from './components/Header'
import Hero from './components/Hero'
import StatsBar from './components/StatsBar'
import FeatureGrid from './components/FeatureGrid'
import { TUIFeature, GDBFeature, P2PFeature, CIFeature } from './components/DeepFeatures'
import InstallSection from './components/InstallSection'
import Footer from './components/Footer'

export default function App() {
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

        <P2PFeature />

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
