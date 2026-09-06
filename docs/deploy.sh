#!/bin/bash

# This script deploys the Docusaurus site to GitHub Pages

# Exit on error
set -e

# Build the site
echo "Building the site..."
npm run build

# Create or use the gh-pages branch
echo "Preparing for deployment..."
git checkout -b gh-pages-temp
git add -f build
git commit -m "Deploy website - $(date)"

# Force push to the gh-pages branch
echo "Deploying to GitHub Pages..."
git subtree split --prefix build -b gh-pages
git push -f origin gh-pages:gh-pages

# Clean up
echo "Cleaning up..."
git checkout main
git branch -D gh-pages-temp
git branch -D gh-pages

echo "Deployment complete! Your site should be available at https://postie.kipsilabs.top/" 