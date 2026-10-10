//! Resident receipts couple immutable roots to the actual persistent maps.
//! Historical declaration and context blocks are acquired only on demand.
use super::*;
use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use imbl::OrdMap;
use parking_lot::Mutex;
use tidepool_toolchain::exact_context::{
    hydrate_context, hydrate_map, read_value, MaterializedContext, MaterializedMap,
    Materializer, SnapshotMapRoot,
};
use crate::session::recovery_store::{
    DomainSnapshotRoot, PublicSurfaceRoot, PreparedRecoveryWrite,
    PreparedPublicOwnerChange, RecoveryPublicOwnerState,
};

const VERSION:u32=1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainSnapshot {
    version:u32,
    source_session:u64,
    nodes:SnapshotMapRoot,
    artifacts:SnapshotMapRoot,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceSnapshot {
    version:u32,
    #[serde(with="option_generation_serde")]
    declaration_root:Option<Generation>,
    context_root:Option<ContextRoot>,
    bindings:SnapshotMapRoot,
    source_instances:SnapshotMapRoot,
}
#[derive(Clone, Debug)]
pub(crate) struct RecoveryPublicSurface {
    pub owner:RecoveryPublicOwner,
    pub declaration_root:Option<Generation>,
    pub epoch:u64,
    pub context_root:Option<ContextRoot>,
    pub bindings:OrdMap<String,RecoveryBindingId>,
    pub source_instances:OrdMap<RecoveryPublicSourceInstance,()>,
    binding_receipt:Option<MaterializedMap<String,RecoveryBindingId>>,
    source_receipt:Option<MaterializedMap<RecoveryPublicSourceInstance,()>>,
}
impl RecoveryPublicSurface {
    pub(crate) fn empty(owner:RecoveryPublicOwner)->Self {
        Self {owner,declaration_root:None,epoch:0,context_root:None,bindings:OrdMap::new(),source_instances:OrdMap::new(),binding_receipt:None,source_receipt:None}
    }
    /// Apply the owning visibility snapshot to the actual predecessor trees.
    /// Retained values stay shared and the materializer sees only map changes.
    pub(crate) fn set_visibility(&mut self,bindings:impl IntoIterator<Item=RecoveryPublicBinding>,sources:impl IntoIterator<Item=RecoveryPublicSourceInstance>) {
        let mut next=std::collections::BTreeMap::new();
        for binding in bindings {next.insert(binding.name,binding.owner);}
        self.bindings.retain(|name,_|next.contains_key(name));
        for (name,owner) in next {if self.bindings.get(&name)!=Some(&owner){self.bindings.insert(name,owner);}}
        let next:std::collections::BTreeSet<_>=sources.into_iter().collect();
        self.source_instances.retain(|source,_|next.contains(source));
        for source in next {if !self.source_instances.contains_key(&source){self.source_instances.insert(source,());}}
    }
}

#[derive(Default,Debug)]
struct Acquired {
    nodes:BTreeMap<Generation,Arc<RecoveryNode>>,
    artifacts:BTreeMap<ArtifactId,Arc<RecoveryArtifactClosure>>,
    surfaces:BTreeMap<(RecoveryPublicOwner,u64,Cid),Arc<RecoveryPublicSurface>>,
    contexts:BTreeMap<ContextRoot,Arc<MaterializedContext>>,
}

#[derive(Clone)]
pub(crate) struct RecoveryGraph {
    store:Arc<RunRecoveryStore>,
    domain:RecoveryDomainId,
    source_session:u64,
    lineage:u64,
    revision:StoreRevision,
    root:Option<DomainSnapshotRoot>,
    ceiling:Generation,
    nodes:OrdMap<u64,Cid>,
    artifacts:OrdMap<ArtifactId,Cid>,
    node_receipt:Option<MaterializedMap<u64,Cid>>,
    artifact_receipt:Option<MaterializedMap<ArtifactId,Cid>>,
    owner_roots:OrdMap<RecoveryPublicOwner,RecoveryPublicOwnerState>,
    /// Only newly prepared immutable bytes, retained until transaction settlement.
    pending:OrdMap<Cid,Arc<[u8]>>,
    public_changes:Vec<PreparedPublicOwnerChange>,
    acquired:Arc<Mutex<Acquired>>,
}
impl std::fmt::Debug for RecoveryGraph {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        f.debug_struct("RecoveryGraph").field("domain",&self.domain).field("revision",&self.revision).field("nodes",&self.nodes.len()).field("artifacts",&self.artifacts.len()).finish()
    }
}
impl Blockstore for RecoveryGraph {
    fn get(&self,cid:&Cid)->anyhow::Result<Option<Vec<u8>>> {
        self.store.ensure_ready()?;
        if let Some(bytes)=self.pending.get(cid){return Ok(Some(bytes.to_vec()));}
        self.store.get(cid)
    }
    fn put_keyed(&self,_:&Cid,_:&[u8])->anyhow::Result<()> {anyhow::bail!("recovery snapshots require an operation-local materializer")}
}
impl RecoveryGraph {
    pub(crate) fn empty(store:Arc<RunRecoveryStore>,domain:RecoveryDomainId,source_session:u64)->Self {
        Self {store,domain,source_session,lineage:source_session,revision:StoreRevision(0),root:None,ceiling:Generation(0),nodes:OrdMap::new(),artifacts:OrdMap::new(),node_receipt:None,artifact_receipt:None,owner_roots:OrdMap::new(),pending:OrdMap::new(),public_changes:Vec::new(),acquired:Arc::new(Mutex::new(Acquired::default()))}
    }
    pub(crate) fn hydrate_domain(store:Arc<RunRecoveryStore>,domain:RecoveryDomainId,state:&RecoveryDomainState)->Result<Self,RecoveryError> {
        store.ensure_ready()?;
        let snapshot:DomainSnapshot=read_value(store.as_ref(),state.snapshot_root.cid())?;
        if snapshot.version!=VERSION {return Err(error("unsupported domain snapshot version"));}
        let nodes=hydrate_map::<u64,Cid,_>(store.as_ref(),&snapshot.nodes)?;
        let artifacts=hydrate_map::<ArtifactId,Cid,_>(store.as_ref(),&snapshot.artifacts)?;
        let owners=store.public_owner_states(domain)?;
        let graph=Self {source_session:snapshot.source_session,lineage:state.lineage,revision:state.revision,root:Some(state.snapshot_root.clone()),ceiling:state.allocation_ceiling,nodes:nodes.facts().clone(),artifacts:artifacts.facts().clone(),node_receipt:Some(nodes),artifact_receipt:Some(artifacts),owner_roots:owners.into_iter().map(|owner|(owner.owner.clone(),owner)).collect(),store,domain,pending:OrdMap::new(),public_changes:Vec::new(),acquired:Arc::new(Mutex::new(Acquired::default()))};
        Ok(graph)
    }
    pub(crate) fn store(&self)->&Arc<RunRecoveryStore>{&self.store}
    pub(crate) fn domain_id(&self)->RecoveryDomainId {self.domain}
    pub(crate) fn store_revision(&self)->StoreRevision {self.revision}
    pub(crate) fn high_water(&self)->Generation {self.ceiling}
    pub(crate) fn source_session(&self)->u64 {self.source_session}
    pub(crate) fn lineage(&self)->u64 {self.lineage}
    pub(crate) fn node_ids(&self)->impl Iterator<Item=Generation>+'_ {self.nodes.keys().copied().map(Generation)}
    pub(crate) fn artifact_ids(&self)->impl Iterator<Item=ArtifactId>+'_ {self.artifacts.keys().copied()}
    pub(crate) fn public_owners(&self)->impl Iterator<Item=&RecoveryPublicOwner> {self.owner_roots.keys()}
    pub(crate) fn public_owner_state(&self,owner:&RecoveryPublicOwner)->Option<&RecoveryPublicOwnerState>{self.owner_roots.get(owner)}
    pub(crate) fn load_public_surfaces(&self)->Result<(),RecoveryError> {for owner in self.public_owners(){self.surface(owner)?;} Ok(())}
    pub(crate) fn public_surfaces(&self)->impl Iterator<Item=Arc<RecoveryPublicSurface>>+'_ {
        // Attachment acquires active surfaces before exposing this resident view.
        let acquired=self.acquired.lock();
        self.owner_roots.keys().filter_map(|owner|acquired.surfaces.get(&(owner.clone(),self.owner_roots[owner].epoch,*self.owner_roots[owner].surface_root.cid())).cloned()).collect::<Vec<_>>().into_iter()
    }
    pub(crate) fn node(&self,id:Generation)->Result<Option<Arc<RecoveryNode>>,RecoveryError> {
        self.store.ensure_ready()?;
        let Some(cid)=self.nodes.get(&id.0) else{return Ok(None)};
        if let Some(node)=self.acquired.lock().nodes.get(&id).cloned(){return Ok(Some(node));}
        let node:RecoveryNode=read_value(self,cid)?;
        if node.id!=id||node.parent.is_some_and(|parent|parent>=id)||node.implementation_refs.iter().any(|dependency|*dependency>=id){return Err(error("declaration block identity or causal ancestry is invalid"));}
        let node=Arc::new(node); self.acquired.lock().nodes.insert(id,node.clone()); Ok(Some(node))
    }
    pub(crate) fn artifact(&self,id:ArtifactId)->Result<Option<Arc<RecoveryArtifactClosure>>,RecoveryError> {
        self.store.ensure_ready()?;
        let Some(cid)=self.artifacts.get(&id) else{return Ok(None)};
        if let Some(artifact)=self.acquired.lock().artifacts.get(&id).cloned(){return Ok(Some(artifact));}
        let artifact:RecoveryArtifactClosure=read_value(self,cid)?;
        if artifact.artifact_id()!=id||artifact.declared_artifact_id()!=id||artifact.component_paths().iter().any(|path|!relative(path)){return Err(error("artifact block identity or custody path is invalid"));}
        let artifact=Arc::new(artifact); self.acquired.lock().artifacts.insert(id,artifact.clone()); Ok(Some(artifact))
    }
    pub(crate) fn context(&self,root:&ContextRoot)->Result<Arc<MaterializedContext>,RecoveryError> {
        self.store.ensure_ready()?;
        if let Some(context)=self.acquired.lock().contexts.get(root).cloned(){return Ok(context)}
        let context=Arc::new(hydrate_context(self,root)?);
        self.acquired.lock().contexts.insert(root.clone(),context.clone()); Ok(context)
    }
    pub(crate) fn remember_context(&self,context:Arc<MaterializedContext>)->Result<(),RecoveryError> {
        self.store.ensure_ready()?;
        self.acquired.lock().contexts.insert(context.root().clone(),context); Ok(())
    }
    pub(crate) fn surface(&self,owner:&RecoveryPublicOwner)->Result<Option<Arc<RecoveryPublicSurface>>,RecoveryError> {
        self.store.ensure_ready()?;
        let Some(header)=self.owner_roots.get(owner) else{return Ok(None)};
        if let Some(surface)=self.acquired.lock().surfaces.get(&(owner.clone(),header.epoch,*header.surface_root.cid())).cloned(){return Ok(Some(surface));}
        let snapshot:SurfaceSnapshot=read_value(self,header.surface_root.cid())?;
        if snapshot.version!=VERSION||snapshot.declaration_root.is_some_and(|id|!self.nodes.contains_key(&id.0)){return Err(error("public surface has invalid version or declaration root"));}
        let bindings=hydrate_map::<String,RecoveryBindingId,_>(self,&snapshot.bindings)?;
        let sources=hydrate_map::<RecoveryPublicSourceInstance,(),_>(self,&snapshot.source_instances)?;
        let surface=Arc::new(RecoveryPublicSurface {owner:owner.clone(),epoch:header.epoch,declaration_root:snapshot.declaration_root,context_root:snapshot.context_root,bindings:bindings.facts().clone(),source_instances:sources.facts().clone(),binding_receipt:Some(bindings),source_receipt:Some(sources)});
        self.acquired.lock().surfaces.insert((owner.clone(),header.epoch,*header.surface_root.cid()),surface.clone()); Ok(Some(surface))
    }
    pub(crate) fn candidate(&self)->RecoveryGraphCandidate {
        RecoveryGraphCandidate {graph:self.clone()}
    }
    pub(crate) fn domain_root(&self)->Option<&DomainSnapshotRoot>{self.root.as_ref()}
    pub(crate) fn public_surface_root(&self,owner:&RecoveryPublicOwner)->Option<&PublicSurfaceRoot>{self.owner_roots.get(owner).map(|header|&header.surface_root)}
    pub(crate) fn materialize_domain(mut self)->Result<(Self,DomainSnapshotRoot,Vec<(Cid,Arc<[u8]>)>),RecoveryError> {
        let mut materializer=Materializer::new(&self);
        let nodes=materializer.materialize_map(self.node_receipt.as_ref(),&self.nodes)?;
        let artifacts=materializer.materialize_map(self.artifact_receipt.as_ref(),&self.artifacts)?;
        let root=DomainSnapshotRoot::new(materializer.materialize_value(&DomainSnapshot{version:VERSION,source_session:self.source_session,nodes:nodes.root().clone(),artifacts:artifacts.root().clone()})?);
        let packet=materializer.finish()?;
        self.node_receipt=Some(nodes); self.artifact_receipt=Some(artifacts);
        for (cid,bytes) in packet.into_blocks(){self.pending.insert(cid,bytes);}
        let blocks=self.pending.iter().map(|(cid,bytes)|(*cid,bytes.clone())).collect();
        Ok((self,root,blocks))
    }
    pub(crate) fn install_store_state(&mut self,state:&RecoveryDomainState) {
        self.revision=state.revision; self.ceiling=state.allocation_ceiling; self.lineage=state.lineage; self.root=Some(state.snapshot_root.clone()); self.pending.clear(); self.public_changes.clear();
    }
}

pub(crate) struct RecoveryGraphCandidate {graph:RecoveryGraph}
impl RecoveryGraphCandidate {
    fn encode<T:Serialize>(&mut self,value:&T)->Result<Cid,RecoveryError> {
        let mut materializer=Materializer::new(&self.graph);
        let cid=materializer.materialize_value(value)?;
        let packet=materializer.finish()?;
        for (cid,bytes) in packet.into_blocks(){self.graph.pending.insert(cid,bytes);}
        Ok(cid)
    }
    pub(crate) fn insert_node(&mut self,node:RecoveryNode)->Result<(),RecoveryError> {
        if node.id.0==0||self.graph.nodes.contains_key(&node.id.0)||node.parent.is_some_and(|id|id>=node.id||!self.graph.nodes.contains_key(&id.0))||node.implementation_refs.iter().any(|id|*id>=node.id||!self.graph.nodes.contains_key(&id.0)){return Err(error("declaration insertion violates immutable causal identity"));}
        let cid=self.encode(&node)?; self.graph.nodes.insert(node.id.0,cid); self.graph.acquired.lock().nodes.insert(node.id,Arc::new(node)); Ok(())
    }
    pub(crate) fn insert_artifact(&mut self,artifact:RecoveryArtifactClosure)->Result<(),RecoveryError> {
        let id=artifact.artifact_id();
        if artifact.declared_artifact_id()!=id||artifact.component_paths().iter().any(|path|!relative(path)){return Err(error("artifact insertion has invalid identity or custody path"));}
        if let Some(existing)=self.graph.artifact(id)? {if *existing!=artifact{return Err(error("artifact identity cannot replace immutable custody facts"));} return Ok(())}
        let cid=self.encode(&artifact)?; self.graph.artifacts.insert(id,cid); self.graph.acquired.lock().artifacts.insert(id,Arc::new(artifact)); Ok(())
    }
    pub(crate) fn insert_context(&mut self,context:&tidepool_toolchain::declaration_join::ExactDeclarationContext,previous:Option<&ContextRoot>)->Result<ContextRoot,RecoveryError> {
        let base=previous.map(|root|self.graph.context(root)).transpose()?;
        let mut materializer=Materializer::new(&self.graph);
        let context=Arc::new(materializer.materialize_context(base.as_deref(),&context.snapshot())?);
        let packet=materializer.finish()?;
        for (cid,bytes) in packet.into_blocks(){self.graph.pending.insert(cid,bytes);}
        let root=context.root().clone(); self.graph.remember_context(context)?; Ok(root)
    }
    pub(crate) fn replace_surface(&mut self,mut surface:RecoveryPublicSurface)->Result<(),RecoveryError> {
        let previous=self.graph.owner_roots.get(&surface.owner).cloned();
        let mut materializer=Materializer::new(&self.graph);
        let bindings=materializer.materialize_map(surface.binding_receipt.as_ref(),&surface.bindings)?;
        let sources=materializer.materialize_map(surface.source_receipt.as_ref(),&surface.source_instances)?;
        let root=PublicSurfaceRoot::new(materializer.materialize_value(&SurfaceSnapshot {version:VERSION,declaration_root:surface.declaration_root,context_root:surface.context_root.clone(),bindings:bindings.root().clone(),source_instances:sources.root().clone()})?);
        let packet=materializer.finish()?;
        surface.binding_receipt=Some(bindings); surface.source_receipt=Some(sources);
        for (cid,bytes) in packet.into_blocks(){self.graph.pending.insert(cid,bytes);}
        let next=RecoveryPublicOwnerState{owner:surface.owner.clone(),epoch:surface.epoch,surface_root:root};
        self.graph.public_changes.push(match previous {Some(previous)=>PreparedPublicOwnerChange::Replace{expected_epoch:previous.epoch,next:next.clone()},None=>PreparedPublicOwnerChange::Insert(next.clone())});
        self.graph.owner_roots.insert(next.owner.clone(),next.clone());
        self.graph.acquired.lock().surfaces.insert((surface.owner.clone(),surface.epoch,*next.surface_root.cid()),Arc::new(surface)); Ok(())
    }
    pub(crate) fn seal(self)->Result<RecoveryGraph,RecoveryError>{Ok(self.graph)}
    pub(crate) fn prepare(self,expected:StoreRevision,ceiling:Generation)->Result<StagedRecoveryManifest,RecoveryError> {
        let (graph,root,blocks)=self.graph.materialize_domain()?;
        let work=crate::session::RecoveryPublicationWork{manifest_write_bytes:blocks.iter().map(|(_,bytes)|bytes.len() as u64).sum(),..Default::default()};
        let prepared=PreparedRecoveryWrite{domain_id:graph.domain,expected_revision:expected,snapshot_root:root,allocation_ceiling:ceiling,blocks,public_changes:graph.public_changes.clone()};
        Ok(StagedRecoveryManifest{graph,prepared,work})
    }
}

pub(crate) struct StagedRecoveryManifest {
    pub(crate) graph:RecoveryGraph,
    pub(crate) prepared:PreparedRecoveryWrite,
    pub(crate) work:crate::session::RecoveryPublicationWork,
}
impl StagedRecoveryManifest {
    pub(crate) fn publish_and_install<T>(self,install:impl FnOnce(RecoveryGraph,RecoveryDomainState)->T)->StoreDisposition<T> {
        let Self{mut graph,prepared,..}=self;
        let store=graph.store.clone();
        store.publish_and_install(prepared,move |state|{graph.install_store_state(&state);install(graph,state)})
    }
}
