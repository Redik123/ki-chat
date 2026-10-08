//! Le port série, par l'API Windows — sans bibliothèque de plus : celle du
//! port série courante est sous MPL-2.0, que la politique de licences du
//! dépôt (`deny.toml`) n'accepte pas.
//!
//! Ouvrir le COM du Loupedeck, le régler comme un port USB série ordinaire
//! (8 bits, sans parité, sans contrôle de flux, DTR levé — le débit est
//! symbolique en USB), lire avec un délai, écrire ; et trouver lequel des
//! ports présents est le sien. Ailleurs que sous Windows, l'appareil n'est
//! pas piloté : les mêmes noms existent, et ne trouvent rien.

#[cfg(windows)]
pub use windows_impl::{trouver, Port};

#[cfg(not(windows))]
pub use ailleurs::{trouver, Port};

#[cfg(windows)]
mod windows_impl {
    use std::time::Duration;

    use anyhow::{anyhow, bail, Context, Result};
    use windows::core::{w, GUID, PCWSTR};
    use windows::Win32::Devices::Communication::{
        EscapeCommFunction, GetCommState, PurgeComm, SetCommState, SetCommTimeouts, COMMTIMEOUTS, DCB, NOPARITY,
        ONESTOPBIT, PURGE_COMM_FLAGS, PURGE_RXCLEAR, PURGE_TXCLEAR, SETDTR,
    };
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
        SetupDiOpenDevRegKey, DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, GUID_DEVCLASS_PORTS, SPDRP_HARDWAREID,
        SP_DEVINFO_DATA,
    };
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING,
    };
    use windows::Win32::System::Registry::{RegCloseKey, RegQueryValueExW, KEY_READ};

    /// Le fabricant Loupedeck, dans l'identifiant matériel USB.
    const VID: &str = "VID_2EC2";
    /// Une écriture ne bloque jamais le fil plus que ça.
    const ECRITURE_MAX_MS: u32 = 2000;

    /// Un port COM ouvert.
    pub struct Port {
        handle: HANDLE,
    }

    // Une poignée de port COM s'utilise de n'importe quel fil ; le pilote
    // ne s'en sert que d'un seul à la fois.
    unsafe impl Send for Port {}
    unsafe impl Sync for Port {}

    impl Port {
        pub fn ouvrir(nom: &str, bauds: u32) -> Result<Self> {
            let chemin: Vec<u16> = format!(r"\\.\{nom}").encode_utf16().chain(Some(0)).collect();
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(chemin.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_err(|e| {
                if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
                    anyhow!("{nom} est déjà ouvert par un autre programme (le logiciel Loupedeck est-il fermé ?)")
                } else {
                    anyhow!("ouverture de {nom} : {e}")
                }
            })?;
            // Désormais fermé quoi qu'il arrive, même si un réglage échoue.
            let port = Port { handle };

            let mut dcb = DCB { DCBlength: size_of::<DCB>() as u32, ..Default::default() };
            unsafe { GetCommState(handle, &mut dcb) }.context("lecture des réglages du port")?;
            dcb.BaudRate = bauds;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;
            // fBinary seul : ni parité, ni contrôle de flux matériel ou
            // logiciel, DTR et RTS menés à la main, rien d'avalé ni
            // d'abandonné sur erreur.
            dcb._bitfield = 1;
            dcb.XonChar = 17;
            dcb.XoffChar = 19;
            dcb.ErrorChar = 0;
            dcb.EofChar = 26;
            unsafe { SetCommState(handle, &dcb) }.context("réglage du port")?;
            // DTR levé : « l'hôte est là ». Au mieux : un port qui ne le gère
            // pas marche quand même.
            let _ = unsafe { EscapeCommFunction(handle, SETDTR) };
            port.delai(Duration::from_millis(2000))?;
            Ok(port)
        }

        /// Combien une lecture attend son premier octet.
        pub fn delai(&self, d: Duration) -> Result<()> {
            let ms = (d.as_millis().min(u32::MAX as u128 - 1) as u32).max(1);
            // Avec ces trois valeurs, une lecture rend tout de suite ce qui
            // est arrivé ; s'il n'y a rien, elle attend le premier octet au
            // plus `ms`, et rend zéro octet si rien n'est venu.
            let delais = COMMTIMEOUTS {
                ReadIntervalTimeout: u32::MAX,
                ReadTotalTimeoutMultiplier: u32::MAX,
                ReadTotalTimeoutConstant: ms,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: ECRITURE_MAX_MS,
            };
            unsafe { SetCommTimeouts(self.handle, &delais) }.context("délais du port")
        }

        /// Jette ce qui attendait, dans les deux sens.
        pub fn vider(&self) {
            let _ = unsafe { PurgeComm(self.handle, PURGE_COMM_FLAGS(PURGE_RXCLEAR.0 | PURGE_TXCLEAR.0)) };
        }

        /// Ce qui est arrivé, ou zéro octet si rien n'est venu le temps du
        /// délai. Une erreur : l'appareil n'est plus là.
        pub fn lire(&self, tampon: &mut [u8]) -> Result<usize> {
            let mut lus = 0u32;
            unsafe { ReadFile(self.handle, Some(tampon), Some(&mut lus as *mut u32), None) }
                .context("lecture du port")?;
            Ok(lus as usize)
        }

        /// Tout, ou une erreur.
        pub fn ecrire(&self, mut octets: &[u8]) -> Result<()> {
            while !octets.is_empty() {
                let mut ecrits = 0u32;
                unsafe { WriteFile(self.handle, Some(octets), Some(&mut ecrits as *mut u32), None) }
                    .context("écriture sur le port")?;
                if ecrits == 0 {
                    bail!("le port n'écrit plus (appareil débranché ?)");
                }
                octets = &octets[ecrits as usize..];
            }
            Ok(())
        }
    }

    impl Drop for Port {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    /// Le nom (« COM3 ») du port d'un appareil Loupedeck branché : parmi les
    /// ports série présents, celui dont l'identifiant matériel porte le VID
    /// de Loupedeck ; son nom est dans sa clé de registre.
    pub fn trouver() -> Result<String> {
        unsafe {
            let infos = SetupDiGetClassDevsW(
                Some(&GUID_DEVCLASS_PORTS as *const GUID),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT,
            )
            .context("liste des ports série")?;
            let mut trouve = None;
            for i in 0.. {
                let mut appareil = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
                if SetupDiEnumDeviceInfo(infos, i, &mut appareil).is_err() {
                    break;
                }
                let mut ids = [0u8; 1024];
                let lu = SetupDiGetDeviceRegistryPropertyW(infos, &appareil, SPDRP_HARDWAREID, None, Some(&mut ids[..]), None);
                if lu.is_err() || !utf16(&ids).to_uppercase().contains(VID) {
                    continue;
                }
                let Ok(cle) = SetupDiOpenDevRegKey(infos, &appareil, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, KEY_READ.0) else {
                    continue;
                };
                let mut nom = [0u8; 128];
                let mut taille = nom.len() as u32;
                let lu = RegQueryValueExW(
                    cle,
                    w!("PortName"),
                    None,
                    None,
                    Some(nom.as_mut_ptr()),
                    Some(&mut taille as *mut u32),
                );
                let _ = RegCloseKey(cle);
                let nom = utf16(&nom[..(taille as usize).min(nom.len())]);
                if lu.is_ok() && !nom.is_empty() {
                    trouve = Some(nom);
                    break;
                }
            }
            let _ = SetupDiDestroyDeviceInfoList(infos);
            trouve.context("aucun Loupedeck branché (VID 2EC2)")
        }
    }

    /// Des octets UTF-16 petit-boutistes, jusqu'au premier caractère nul.
    /// Pour une liste (les identifiants matériels), c'est le premier — qui
    /// porte le VID comme les autres.
    fn utf16(octets: &[u8]) -> String {
        let mots: Vec<u16> = octets.as_chunks::<2>().0.iter().map(|paire| u16::from_le_bytes(*paire)).collect();
        let fin = mots.iter().position(|m| *m == 0).unwrap_or(mots.len());
        String::from_utf16_lossy(&mots[..fin])
    }
}

#[cfg(not(windows))]
mod ailleurs {
    use std::time::Duration;

    use anyhow::{bail, Result};

    pub struct Port;

    impl Port {
        pub fn ouvrir(_nom: &str, _bauds: u32) -> Result<Self> {
            bail!("le Loupedeck n'est piloté que sous Windows")
        }
        pub fn delai(&self, _d: Duration) -> Result<()> {
            Ok(())
        }
        pub fn vider(&self) {}
        pub fn lire(&self, _tampon: &mut [u8]) -> Result<usize> {
            Ok(0)
        }
        pub fn ecrire(&self, _octets: &[u8]) -> Result<()> {
            Ok(())
        }
    }

    pub fn trouver() -> Result<String> {
        bail!("le Loupedeck n'est piloté que sous Windows")
    }
}
