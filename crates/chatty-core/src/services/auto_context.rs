use std::path::Path;

use tracing::{info, warn};

use super::{
    embedding_service::EmbeddingService, memory_service::MemoryService, skill_service::SkillService,
};
use crate::tools::{build_memory_context_block, merge_search_results, select_context_hits};

pub struct AutoContextRequest<'a> {
    pub memory_service: &'a MemoryService,
    pub embedding_service: Option<&'a EmbeddingService>,
    pub skill_service: &'a SkillService,
    pub query_text: &'a str,
    pub fallback_query_text: Option<&'a str>,
    pub workspace_skills_dir: Option<&'a Path>,
}

pub async fn load_auto_context_block(request: AutoContextRequest<'_>) -> Option<String> {
    let fallback_query = request
        .fallback_query_text
        .filter(|query| !query.is_empty() && *query != request.query_text);

    let search_text = if request.query_text.is_empty() {
        fallback_query.unwrap_or_default()
    } else {
        request.query_text
    };
    let bm25_query = (!request.query_text.is_empty())
        .then_some(request.query_text)
        .or(fallback_query);

    let bm25_search = async {
        let Some(query) = bm25_query else {
            return Vec::new();
        };

        match request.memory_service.search(query, Some(3)).await {
            Ok(hits) if !hits.is_empty() => hits,
            Ok(_) => {
                if let Some(fallback) = fallback_query.filter(|fallback| *fallback != query) {
                    info!("Simplified query got 0 hits, retrying with fallback text");
                    request
                        .memory_service
                        .search(fallback, Some(3))
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            Err(e) => {
                warn!(error = ?e, "Memory auto-retrieval BM25 failed (non-fatal)");
                Vec::new()
            }
        }
    };

    let vector_search = async {
        if let Some(embed_svc) = request.embedding_service
            && !search_text.is_empty()
        {
            match embed_svc.embed(search_text).await {
                Ok(query_embedding) => {
                    let hits = request
                        .memory_service
                        .search_vec(query_embedding.clone(), Some(3))
                        .await
                        .unwrap_or_default();
                    return (hits, Some(query_embedding));
                }
                Err(e) => {
                    warn!(error = ?e, "Memory auto-retrieval embedding failed (non-fatal)");
                }
            }
        }

        (Vec::new(), None)
    };

    let (bm25_hits, (vec_hits, query_embedding_opt)) = tokio::join!(bm25_search, vector_search);

    let memory_hits = merge_search_results(bm25_hits, vec_hits, 5);
    let skill_hits = if search_text.is_empty() {
        Vec::new()
    } else {
        request
            .skill_service
            .load_hits(
                search_text,
                query_embedding_opt.as_deref(),
                request.workspace_skills_dir,
            )
            .await
    };

    let hits = select_context_hits(memory_hits, skill_hits, 5);
    build_memory_context_block(hits)
}
