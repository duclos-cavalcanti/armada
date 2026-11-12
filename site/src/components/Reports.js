import Layout from '@theme/Layout';

// The skills' reports, newest first. Each is a whole HTML page of its own, so a plain link: a full
// page load, not a route of the site.
export default function Reports({ reports }) {
  return (
    <Layout title="Reports">
      <main className="container margin-vert--lg">
        <h1>Reports</h1>
        {reports.length === 0 ? (
          <p>
            None yet: the skills (<code>/fleet-convoy-report</code>, <code>/fleet-cost-report</code>, …) write
            them to <code>reports/&lt;project&gt;/</code>.
          </p>
        ) : (
          <ul>
            {reports.map(({ file, mtime }) => (
              <li key={file}>
                <a href={`/reports/${file}`}>{file}</a>{' '}
                <small>{new Date(mtime).toLocaleString()}</small>
              </li>
            ))}
          </ul>
        )}
      </main>
    </Layout>
  );
}
