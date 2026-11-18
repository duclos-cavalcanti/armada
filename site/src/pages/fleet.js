import Layout from '@theme/Layout';
import BrowserOnly from '@docusaurus/BrowserOnly';

// The fleet viewer (demos/fleet) under the site's navbar, filling the rest of the window: it lays
// its panels out against its own box, so that box is the viewport less the navbar. Browser-only:
// it reads window.location as it loads.
export default function Fleet() {
  return (
    <Layout title="Fleet viewer" noFooter>
      <div style={{ height: 'calc(100vh - var(--ifm-navbar-height))' }}>
        <BrowserOnly>
          {() => {
            const App = require('../../../demos/fleet/src/App.jsx').default;
            return <App />;
          }}
        </BrowserOnly>
      </div>
    </Layout>
  );
}
