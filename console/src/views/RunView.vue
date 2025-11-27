<script setup lang="ts">
import 'splitpanes/dist/splitpanes.css'
import { Splitpanes, Pane } from 'splitpanes'
import InputsComponent from '@/components/InputsComponent.vue';
import DiagramComponent from '@/components/DiagramComponent.vue';
import LogsComponent from '@/components/LogsComponent.vue';
import TreeComponent from '@/components/TreeComponent.vue';
import VncComponent from '@/components/VncComponent.vue';
</script>

<template>
  <Splitpanes class="main-body" horizontal>
    <Pane min-size="20">
      <Splitpanes>
        <Pane min-size="20">
          <div class="panel inputs">
            <InputsComponent></InputsComponent>
          </div>
        </Pane>

        <Pane min-size="20">
          <div class="panel">
            <p>VNC is not available yet (coming soon)</p>
            <!-- <VncComponent></VncComponent> -->
            <!-- <DiagramComponent></DiagramComponent> -->
          </div>
        </Pane>
      </Splitpanes>
    </Pane>

    <Pane min-size="20">
      <Splitpanes>
        <Pane min-size="20">
          <div class="panel logs">
            <LogsComponent></LogsComponent>
          </div>
        </Pane>
        <!-- <Pane min-size="20">
          <div class="panel logs">
            <TreeComponent msg="Hello"></TreeComponent>
          </div>
        </Pane> -->
      </Splitpanes>
    </Pane>
  </Splitpanes>
</template>

<style scoped>
.app-container {
  display: flex;
  flex-direction: column;
  height: 100vh;
}

/* This fills remaining space below the navbar */
.main-body {
  flex: 1;
  height: 100%;
}

/* Optional panel styling */
.panel {
  height: 100%;
  width: 100%;
  overflow: auto;
  border: 1px solid black;
  /* padding: 16px; */
}

.logs {
  background-color: #0d0d18;
  height: 100%;             /* make sure it has some height */
  color: white;
}

.inputs {
  background-color: #ffffff;
}

.diagram {
  background-color: #0d0d18;
  color: white;
}

.deploy {
  background-color: #0d0d18;
  display: flex;
  justify-content: center;  /* center horizontally */
  align-items: center;      /* center vertically */
  height: 100%;             /* make sure it has some height */
}

.deploy button {
  background-color: #7c3aed;   /* vibrant purple */
  color: white;
  font-size: 1rem;
  font-weight: 600;
  padding: 12px 24px;
  border: none;
  border-radius: 8px;
  cursor: pointer;
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.2);
  transition: background-color 0.2s ease;
}

.deploy button:hover {
  background-color: #5b21b6;  /* darker shade on hover */
}

.deploy button:disabled {
  background-color: #a78bfa;
  cursor: not-allowed;
  opacity: 0.7;
}
</style>