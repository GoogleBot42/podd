import { Alert, Link } from '@mui/material';
import FavoriteIcon from '@mui/icons-material/Favorite';

import Header from '../Header.tsx';
import PageContainer from '../../PageContainer.tsx';

/**
 * There is no standalone vitals view yet — the biometrics pipeline that would
 * fill it doesn't exist (#12), and the charts that do exist live on the sleep
 * page next to the night they belong to. `/data/vitals` stays routable because
 * the URL is bookmarkable, but it says so instead of rendering an empty frame
 * (#108). The Data menu deliberately has no entry pointing here.
 */
export default function VitalsPage() {
  return (
    <PageContainer sx={ { mb: 15, gap: 1 } }>
      <Header title="Vitals" icon={ <FavoriteIcon /> }/>
      <Alert severity="info" sx={ { mt: 2 } }>
        Vitals aren&apos;t recorded yet. Heart rate, HRV and breathing rate show up
        under <Link href="/data/sleep">Data → Sleep</Link> once a night has been
        analyzed.
      </Alert>
    </PageContainer>
  );
}
