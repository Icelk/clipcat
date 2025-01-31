use std::{collections::HashMap, time::SystemTime};

#[cfg(feature = "x11")]
use snafu::ResultExt;

use crate::{ClipboardData, ClipboardError, ClipboardType};

const DEFAULT_CAPACITY: usize = 40;

#[cfg_attr(test, allow(dead_code))]
enum Backend {
    #[cfg(test)]
    Test,
    #[cfg(feature = "wayland")]
    Wayland(wl_clipboard_rs::copy::Options),
    #[cfg(feature = "x11")]
    X11(x11_clipboard::Clipboard),
}
pub struct ClipboardManager {
    clips: HashMap<u64, ClipboardData>,
    capacity: usize,
    current_clipboard: Option<ClipboardData>,
    current_primary: Option<ClipboardData>,
    backend: Backend,
}

impl ClipboardManager {
    #[cfg_attr(test, allow(dead_code))]
    fn get_backend() -> Result<Backend, ClipboardError> {
        let b = {
            #[cfg(all(feature = "wayland", feature = "x11"))]
            {
                if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                    Backend::Wayland(wl_clipboard_rs::copy::Options::new())
                } else {
                    Backend::X11(
                        x11_clipboard::Clipboard::new()
                            .context(crate::error::InitializeX11ClipboardSnafu)?,
                    )
                }
            }
            #[cfg(all(feature = "wayland", not(feature = "x11")))]
            {
                Backend::Wayland(wl_clipboard_rs::copy::Options::new())
            }
            #[cfg(all(feature = "x11", not(feature = "wayland")))]
            {
                Backend::X11(
                    x11_clipboard::Clipboard::new()
                        .context(crate::error::InitializeX11ClipboardSnafu)?,
                )
            }
        };
        Ok(b)
    }
    pub fn with_capacity(capacity: usize) -> Result<ClipboardManager, ClipboardError> {
        #[cfg(test)]
        let backend = Backend::Test;
        #[cfg(not(test))]
        let backend = Self::get_backend()?;
        Ok(ClipboardManager {
            capacity,
            clips: HashMap::default(),
            current_clipboard: None,
            current_primary: None,
            backend,
        })
    }

    #[inline]
    pub fn new() -> Result<ClipboardManager, ClipboardError> {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn set_capacity(&mut self, v: usize) {
        self.capacity = v;
    }

    #[inline]
    pub fn import(&mut self, clips: &[ClipboardData]) {
        self.import_iter(clips.iter());
    }

    #[inline]
    pub fn import_iter<'a>(&'a mut self, clips_iter: impl Iterator<Item = &'a ClipboardData>) {
        self.clips = clips_iter.fold(HashMap::new(), |mut clips, clip| {
            clips.insert(clip.id, clip.clone());
            clips
        });
        self.remove_oldest();
    }

    #[inline]
    pub fn list(&self) -> Vec<ClipboardData> {
        self.iter().cloned().collect()
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &ClipboardData> {
        self.clips.values()
    }

    #[inline]
    pub fn get(&self, id: u64) -> Option<ClipboardData> {
        self.clips.get(&id).map(Clone::clone)
    }

    #[inline]
    pub fn get_current_clipboard(&self) -> Option<&ClipboardData> {
        self.current_clipboard.as_ref()
    }

    #[inline]
    pub fn get_current_primary(&self) -> Option<&ClipboardData> {
        self.current_primary.as_ref()
    }

    #[inline]
    pub fn insert(&mut self, data: ClipboardData) -> u64 {
        self.insert_inner(data)
    }

    #[inline]
    pub fn insert_clipboard(&mut self, data: &str) -> u64 {
        let data = ClipboardData::new_clipboard(data);
        self.insert_inner(data)
    }

    #[inline]
    pub fn insert_primary(&mut self, data: &str) -> u64 {
        let data = ClipboardData::new_primary(data);
        self.insert_inner(data)
    }

    fn insert_inner(&mut self, clipboard_data: ClipboardData) -> u64 {
        let id = clipboard_data.id;
        match clipboard_data.clipboard_type {
            ClipboardType::Clipboard => {
                self.current_clipboard = Some(clipboard_data.clone());
            }
            ClipboardType::Primary => {
                self.current_primary = Some(clipboard_data.clone());
            }
        }
        self.clips.insert(clipboard_data.id, clipboard_data);
        self.remove_oldest();
        id
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    fn remove_oldest(&mut self) {
        while self.clips.len() > self.capacity {
            let (_, oldest_id) =
                self.clips
                    .iter()
                    .fold((SystemTime::now(), 0), |oldest, (id, clip)| {
                        if clip.timestamp < oldest.0 {
                            (clip.timestamp, *id)
                        } else {
                            oldest
                        }
                    });

            self.remove(oldest_id);
        }
    }

    #[inline]
    pub fn remove(&mut self, id: u64) -> bool {
        if let Some(clip) = self.current_clipboard.as_ref() {
            if clip.id == id {
                self.current_clipboard.take();
            }
        }

        if let Some(clip) = self.current_primary.as_ref() {
            if clip.id == id {
                self.current_primary.take();
            }
        }

        self.clips.remove(&id).is_some()
    }

    #[inline]
    pub fn clear(&mut self) {
        self.current_clipboard.take();
        self.current_primary.take();
        self.clips.clear();
    }

    pub fn replace(&mut self, old_id: u64, data: &str) -> (bool, u64) {
        let (clipboard_type, timestamp) = match self.clips.remove(&old_id) {
            Some(v) => (v.clipboard_type, v.timestamp),
            None => (ClipboardType::Primary, SystemTime::now()),
        };

        let new_id = ClipboardData::compute_id(data);
        let data = data.to_owned();
        let data = ClipboardData {
            id: new_id,
            data,
            timestamp,
            clipboard_type,
        };

        self.insert_inner(data);
        (true, new_id)
    }

    pub async fn mark_as_clipboard(&mut self, id: u64) -> Result<(), ClipboardError> {
        if let Some(clip) = self.clips.get_mut(&id) {
            clip.mark_as_clipboard();
            let clipboard_content = clip.data.clone();
            self.update_sys_clipboard(&clipboard_content, ClipboardType::Clipboard)?;
        }
        Ok(())
    }

    pub async fn mark_as_primary(&mut self, id: u64) -> Result<(), ClipboardError> {
        if let Some(clip) = self.clips.get_mut(&id) {
            clip.mark_as_primary();
            let clipboard_content = clip.data.clone();
            self.update_sys_clipboard(&clipboard_content, ClipboardType::Primary)?;
        }
        Ok(())
    }

    fn update_sys_clipboard(
        &mut self,
        data: &str,
        clipboard_type: ClipboardType,
    ) -> Result<(), ClipboardError> {
        match &mut self.backend {
            #[cfg(feature = "x11")]
            Backend::X11(cb) => Self::update_sys_clipboard_x11(cb, data, clipboard_type),
            #[cfg(feature = "wayland")]
            Backend::Wayland(cb) => Self::update_sys_clipboard_wayland(cb, data, clipboard_type),
            #[cfg(test)]
            Backend::Test => {
                println!("Update system clipboard {clipboard_type:?} with {data:?}");
                Ok(())
            }
        }
    }

    #[cfg(feature = "wayland")]
    fn update_sys_clipboard_wayland(
        opts: &mut wl_clipboard_rs::copy::Options,
        data: &str,
        clipboard_type: ClipboardType,
    ) -> Result<(), ClipboardError> {
        let cb = match clipboard_type {
            ClipboardType::Clipboard => wl_clipboard_rs::copy::ClipboardType::Regular,
            ClipboardType::Primary => wl_clipboard_rs::copy::ClipboardType::Primary,
        };
        opts.clipboard(cb)
            .clone()
            .copy(
                wl_clipboard_rs::copy::Source::Bytes(data.as_bytes().to_vec().into_boxed_slice()),
                wl_clipboard_rs::copy::MimeType::Text,
            )
            .ok()
            .ok_or(ClipboardError::WaylandWrite)?;
        Ok(())
    }
    #[cfg(feature = "x11")]
    fn update_sys_clipboard_x11(
        clipboard: &x11_clipboard::Clipboard,
        data: &str,
        clipboard_type: ClipboardType,
    ) -> Result<(), ClipboardError> {
        let atom_clipboard = match clipboard_type {
            ClipboardType::Clipboard => clipboard.setter.atoms.clipboard,
            ClipboardType::Primary => clipboard.setter.atoms.primary,
        };
        let atom_utf8string = clipboard.setter.atoms.utf8_string;
        let data = data.to_owned();

        clipboard
            .store(atom_clipboard, atom_utf8string, data.as_bytes())
            .context(crate::error::PasteToX11ClipboardSnafu)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{
        manager::{ClipboardManager, DEFAULT_CAPACITY},
        ClipboardData, ClipboardType,
    };

    fn create_clips(n: usize) -> Vec<ClipboardData> {
        (0..n)
            .map(|i| ClipboardData::new_primary(&i.to_string()))
            .collect()
    }

    #[test]
    fn test_construction() {
        let mgr = ClipboardManager::new().unwrap();
        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
        assert_eq!(mgr.capacity(), DEFAULT_CAPACITY);
        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_none());

        let cap = 20;
        let mgr = ClipboardManager::with_capacity(cap).unwrap();
        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
        assert_eq!(mgr.capacity(), cap);
        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_none());
    }

    #[test]
    fn test_zero_capacity() {
        let mut mgr = ClipboardManager::with_capacity(0).unwrap();
        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
        assert_eq!(mgr.capacity(), 0);

        let n = 20;
        let clips = create_clips(n);
        clips.into_iter().for_each(|clip| {
            mgr.insert(clip);
        });

        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_none());

        let n = 20;
        let clips = create_clips(n);
        mgr.import(&clips);

        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_none());
    }

    #[test]
    fn test_capacity() {
        let cap = 10;
        let mut mgr = ClipboardManager::with_capacity(cap).unwrap();
        assert_eq!(mgr.len(), 0);
        assert_eq!(mgr.capacity(), cap);

        let n = 20;
        let clips = create_clips(n);
        clips.into_iter().for_each(|clip| {
            mgr.insert(clip);
        });

        assert_eq!(mgr.len(), cap);
        assert_eq!(mgr.capacity(), cap);

        let n = 20;
        let clips = create_clips(n);
        mgr.import(&clips);

        assert_eq!(mgr.len(), cap);
        assert_eq!(mgr.capacity(), cap);
    }

    #[test]
    fn test_insert() {
        let n = 20;
        let clips = create_clips(n);
        let mut mgr = ClipboardManager::new().unwrap();
        clips.iter().for_each(|clip| {
            mgr.insert(clip.clone());
        });

        assert!(mgr.get_current_primary().is_some());
        assert_eq!(mgr.get_current_primary(), clips.last());
        assert_eq!(mgr.len(), n);

        let dumped: HashSet<_> = mgr.list().into_iter().collect();
        let clips: HashSet<_> = clips.into_iter().collect();

        assert_eq!(dumped, clips);
    }

    #[test]
    fn test_import() {
        let n = 10;
        let clips = create_clips(n);
        let mut mgr = ClipboardManager::with_capacity(20).unwrap();

        mgr.import(&clips);
        assert_eq!(mgr.len(), n);

        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_none());
        assert_eq!(mgr.len(), n);

        let dumped: HashSet<_> = mgr.list().into_iter().collect();
        let clips: HashSet<_> = clips.into_iter().collect();

        assert_eq!(dumped, clips);
    }

    #[test]
    fn test_replace() {
        let data1 = "ABCDEFG";
        let data2 = "АБВГД";
        let clip = ClipboardData::new_clipboard(data1);
        let mut mgr = ClipboardManager::new().unwrap();
        let old_id = mgr.insert(clip);
        assert_eq!(mgr.len(), 1);

        let (ok, new_id) = mgr.replace(old_id, data2);
        assert!(ok);
        assert_ne!(old_id, new_id);
        assert_eq!(mgr.len(), 1);

        let clip = mgr.get(new_id).unwrap();
        assert_eq!(clip.data, data2);
        assert_eq!(clip.clipboard_type, ClipboardType::Clipboard);
    }

    #[test]
    fn test_remove() {
        let mut mgr = ClipboardManager::new().unwrap();
        assert_eq!(mgr.len(), 0);
        assert!(!mgr.remove(43));

        let clip = ClipboardData::new_primary("АБВГДЕ");
        let id = mgr.insert(clip);
        assert_eq!(mgr.len(), 1);
        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_some());

        let ok = mgr.remove(id);
        assert!(ok);
        assert_eq!(mgr.len(), 0);
        assert!(mgr.get_current_clipboard().is_none());
        assert!(mgr.get_current_primary().is_none());

        let ok = mgr.remove(id);
        assert!(!ok);
    }

    #[test]
    fn test_clear() {
        let n = 20;
        let clips = create_clips(n);
        let mut mgr = ClipboardManager::new().unwrap();

        mgr.import(&clips);
        assert!(!mgr.is_empty());
        assert_eq!(mgr.len(), n);

        mgr.clear();
        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
    }
}
