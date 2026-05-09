---
name: error-handling
description: Improve error handling with proper Result types and context
---

Your job is to improve error handling to production quality. Replace bare .unwrap() with proper Result returns or .expect() with helpful messages. Turn generic Box<dyn Error> into specific error enums using thiserror. Add context when propagating with ?. This is production code running on embedded hardware—errors need to be debuggable from remote logs.
