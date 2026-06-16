import { Routes, Route, Navigate } from 'react-router-dom';
import { Layout } from './ui/Layout';
import { HomePage } from './ui/HomePage';
import { CorePage } from './ui/CorePage';

// Two routes: the catalog index and a per-core detail page. The Layout wraps
// both with the shared header/footer.
export function App() {
  return (
    <Layout>
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/core/:id" element={<CorePage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Layout>
  );
}
