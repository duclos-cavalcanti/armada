import Head from '@docusaurus/Head';
import BrowserOnly from '@docusaurus/BrowserOnly';

// The fleet viewer (demos/fleet), full screen: it lays its panels out against the viewport, so the
// page has no navbar (back to the docs with the browser's Back). Browser-only: it reads
// window.location as it loads.
export default function Fleet() {
  return (
    <>
      <Head>
        <title>Fleet viewer</title>
      </Head>
      <BrowserOnly>
        {() => {
          const App = require('../../../demos/fleet/src/App.jsx').default;
          return <App />;
        }}
      </BrowserOnly>
    </>
  );
}
