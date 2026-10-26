//! I backend di rete. Ognuno traduce il proprio servizio nei tipi neutri di
//! `crate::model`, esegue le `Request` e manda `Response` (eventi live
//! compresi) sullo stesso canale: l'app non vede altro.

pub mod tg;

/// WhatsApp (whatsapp-rust, non ufficiale): solo con `--features whatsapp`.
#[cfg(feature = "whatsapp")]
pub mod wa;
