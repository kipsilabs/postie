import type { ReactNode } from 'react';
import clsx from 'clsx';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import HomepageFeatures from '@site/src/components/HomepageFeatures';
import Heading from '@theme/Heading';

import styles from './index.module.css';

function HomepageHeader() {
  const { siteConfig } = useDocusaurusContext();
  return (
    <header className={clsx('hero hero--primary', styles.heroBanner)}>
      <div className="container">
        <Heading as="h1" className="hero__title">
          {siteConfig.title}
        </Heading>
        <p className="hero__subtitle">{siteConfig.tagline}</p>
        <div className={styles.buttons}>
          <Link
            className="button button--secondary button--lg"
            to="/docs/intro">
            Get Started
          </Link>
          <Link
            className="button button--outline button--lg button--secondary"
            to="https://github.com/kipsilabs/postie"
            style={{ marginLeft: '10px' }}>
            GitHub
          </Link>
        </div>
        <div className={styles.buttons} style={{ marginTop: '20px' }}>
          <Link
            to="https://github.com/kipsilabs/postie/releases/latest/download/postie-gui-windows-amd64.zip"
            style={{ margin: '5px', display: 'inline-block' }}>
            <img 
              src="/img/download-for-windows.webp" 
              alt="Download for Windows" 
              style={{ height: '60px', cursor: 'pointer' }}
            />
          </Link>
          <Link
            to="https://github.com/kipsilabs/postie/releases/latest/download/postie-gui-macos-universal.zip"
            style={{ margin: '5px', display: 'inline-block' }}>
            <img 
              src="/img/download-for-mac.png" 
              alt="Download for macOS" 
              style={{ height: '60px', cursor: 'pointer' }}
            />
          </Link>
        </div>
        <div style={{ marginTop: '15px', textAlign: 'center' }}>
          <Link
            to="https://github.com/kipsilabs/postie/releases/latest"
            style={{ color: 'var(--ifm-hero-text-color)', textDecoration: 'underline' }}>
            View all releases
          </Link>
        </div>
        <div style={{ marginTop: '20px', textAlign: 'center' }}>
          <p>Support the project:</p>
          <a href="https://www.buymeacoffee.com/qbt52hh7sjd">
           <img alt='Buy me a coffe' src="https://img.buymeacoffee.com/button-api/?text=Buy me a coffee&emoji=☕&slug=qbt52hh7sjd&button_colour=FFDD00&font_colour=000000&font_family=Cookie&outline_colour=000000&coffee_colour=ffffff" />
          </a>
        </div>
      </div>
    </header>
  );
}

export default function Home(): ReactNode {
  const { siteConfig } = useDocusaurusContext();
  return (
    <Layout
      title={`${siteConfig.title} - High Performance Usenet Poster`}
      description="A high-performance Usenet binary poster written in Go, inspired by Nyuu-Obfuscation">
      <HomepageHeader />
      <main>
        <section style={{ padding: '2rem 0', backgroundColor: 'var(--ifm-background-color)' }}>
          <div className="container">
            <div style={{ textAlign: 'center', marginBottom: '2rem' }}>
              <Heading as="h2">Screenshots</Heading>
              <p>Modern interface for easy upload management</p>
            </div>
            <div className="row">
              <div className="col col--4">
                <div style={{ textAlign: 'center' }}>
                  <img src="/img/examples/dashboard.png" alt="Dashboard" style={{ width: '100%', borderRadius: '8px', boxShadow: '0 4px 8px rgba(0,0,0,0.1)' }} />
                  <h4>Dashboard</h4>
                  <p>Monitor upload progress and queue status</p>
                </div>
              </div>
              <div className="col col--4">
                <div style={{ textAlign: 'center' }}>
                  <img src="/img/examples/queue.png" alt="Queue Management" style={{ width: '100%', borderRadius: '8px', boxShadow: '0 4px 8px rgba(0,0,0,0.1)' }} />
                  <h4>Queue Management</h4>
                  <p>Control and prioritize your uploads</p>
                </div>
              </div>
              <div className="col col--4">
                <div style={{ textAlign: 'center' }}>
                  <img src="/img/examples/settings.png" alt="Settings" style={{ width: '100%', borderRadius: '8px', boxShadow: '0 4px 8px rgba(0,0,0,0.1)' }} />
                  <h4>Settings</h4>
                  <p>Configure servers and posting options</p>
                </div>
              </div>
            </div>
          </div>
        </section>
        <section style={{ padding: '2rem 0', backgroundColor: 'var(--ifm-color-emphasis-100)' }}>
          <div className="container">
            <div style={{ textAlign: 'center', marginBottom: '2rem' }}>
              <Heading as="h2">Quick Start with Docker</Heading>
              <p>Get Postie running in seconds with Docker</p>
            </div>
            <div style={{ maxWidth: '800px', margin: '0 auto' }}>
              <pre style={{ 
                backgroundColor: 'var(--ifm-code-background)', 
                padding: '1rem', 
                borderRadius: '8px',
                overflow: 'auto'
              }}>
                <code>{`docker run -d \\
  --name postie \\
  -p 8080:8080 \\
  -v ./config:/config \\
  -v ./watch:/watch \\
  -v ./output:/output \\
  ghcr.io/kipsilabs/postie:latest`}</code>
              </pre>
              <div style={{ textAlign: 'center', marginTop: '1rem' }}>
                <p>Then open <strong>http://localhost:8080</strong> in your browser</p>
                <Link
                  className="button button--primary button--lg"
                  to="/docs/installation#docker-installation">
                  Full Docker Setup Guide
                </Link>
              </div>
            </div>
          </div>
        </section>
        <HomepageFeatures />
      </main>
    </Layout>
  );
}
