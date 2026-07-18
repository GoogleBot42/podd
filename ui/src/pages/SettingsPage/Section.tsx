import { PropsWithChildren } from 'react';
import { Typography, Card, CardContent } from '@mui/material';



type SectionProps = PropsWithChildren<{
  title?: string;
}>;

export default function Section({ title, children }: SectionProps) {
  return (
    <Card sx={ { width: '98%', overflowWrap: 'break-word', wordBreak: 'break-word' } }>
      <CardContent>
        {
          title && (
            <>
              <Typography variant='h6' sx={ { textAlign: 'center' } }>
                { title }
              </Typography>
              <br />
            </>
          )
        }
        { children }
      </CardContent>
    </Card>
  );
}
